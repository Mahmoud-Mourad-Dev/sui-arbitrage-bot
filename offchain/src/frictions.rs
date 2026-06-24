//! Frictions model: turn *paper* opportunities (priced authoritatively, but assuming
//! we always land instantly and alone) into a **frictions-adjusted** expectation that
//! accounts for latency and competition — the effects the PnL report explicitly did
//! NOT model (`docs/authoritative-pnl-report.md`, caveat 3).
//!
//! Model (deliberately simple + honest; every parameter is stated, none tuned to a
//! desired answer):
//!
//! 1. **Opportunity survival.** A dislocation of `edge_bps` persists for a half-life
//!    that grows with its size (bigger mispricings take longer to be competed away):
//!    `halflife_ms = halflife_ms_per_bp · edge_bps`. The chance the edge still exists
//!    when our transaction is built + dry-run-gated is the exponential survival
//!    `p_alive = exp(-our_latency_ms / halflife_ms)`.
//! 2. **Race.** Among `competitors` other searchers seeing the same edge, we land
//!    first with `p_win = 1 / (1 + competitors)` (symmetric baseline; a latency edge
//!    would raise this — left as a parameter, not assumed).
//! 3. **Gate interaction.** We only submit if the *fresh* dry-run still clears
//!    `min_profit` (≈ `p_alive`). Given we submit, we either win the race and capture
//!    the profit, or lose it and the on-chain `settle` gate reverts us for gas only:
//!    `E[net] = p_alive · ( p_win · profit − (1 − p_win) · gas )`.
//!
//! This is an estimate, not a guarantee. It exists to answer one question honestly:
//! does the measured edge plausibly survive live frictions enough to justify execution?

/// Tunable frictions assumptions. Defaults are deliberately middle-of-the-road; the
/// report (`frictions-adjusted-pnl.md`) sweeps them.
#[derive(Clone, Copy, Debug)]
pub struct FrictionParams {
    /// Our end-to-end react→land latency (ms): detect + quote + dry-run + submit + land.
    pub our_latency_ms: f64,
    /// Number of competing searchers racing the same dislocation.
    pub competitors: f64,
    /// How long a 1 bp edge survives before being arbed away (ms per bp).
    pub halflife_ms_per_bp: f64,
    /// Gas paid when we submit but lose the race (the trade reverts at `settle`).
    pub gas_cost_usd: f64,
}

impl Default for FrictionParams {
    fn default() -> Self {
        // ~Sui checkpoint cadence for latency; a handful of competitors; edges that
        // last a few hundred ms per bp; a few cents of gas per attempt.
        Self {
            our_latency_ms: 800.0,
            competitors: 3.0,
            halflife_ms_per_bp: 250.0,
            gas_cost_usd: 0.03,
        }
    }
}

/// One paper opportunity: its edge and the authoritatively-priced paper profit.
#[derive(Clone, Copy, Debug)]
pub struct Episode {
    pub edge_bps: f64,
    pub paper_profit_usd: f64,
}

/// Probability the dislocation still exists by the time we're ready to submit.
#[must_use]
pub fn p_alive(edge_bps: f64, p: &FrictionParams) -> f64 {
    if edge_bps <= 0.0 {
        return 0.0;
    }
    let halflife = p.halflife_ms_per_bp * edge_bps;
    (-p.our_latency_ms / halflife).exp()
}

/// Probability we land first among the competitors.
#[must_use]
pub fn p_win(p: &FrictionParams) -> f64 {
    1.0 / (1.0 + p.competitors.max(0.0))
}

/// Expected net USD for one episode after frictions (can be negative: gas on a lost
/// race). The `settle` gate caps the downside at gas — never the trade principal.
#[must_use]
pub fn adjusted_profit(ep: &Episode, p: &FrictionParams) -> f64 {
    let alive = p_alive(ep.edge_bps, p);
    let win = p_win(p);
    alive * (win * ep.paper_profit_usd - (1.0 - win) * p.gas_cost_usd)
}

/// Aggregate paper vs frictions-adjusted expectation over a set of episodes.
#[derive(Clone, Copy, Debug)]
pub struct Aggregate {
    pub episodes: usize,
    pub paper_usd: f64,
    pub adjusted_usd: f64,
    /// adjusted / paper (capture ratio); 0 if paper is 0.
    pub capture_ratio: f64,
}

#[must_use]
pub fn aggregate(episodes: &[Episode], p: &FrictionParams) -> Aggregate {
    let paper: f64 = episodes.iter().map(|e| e.paper_profit_usd).sum();
    let adjusted: f64 = episodes.iter().map(|e| adjusted_profit(e, p)).sum();
    Aggregate {
        episodes: episodes.len(),
        paper_usd: paper,
        adjusted_usd: adjusted,
        capture_ratio: if paper > 0.0 { adjusted / paper } else { 0.0 },
    }
}

// --- liquidation race (harsher than arb) ------------------------------------
//
// Liquidation capture differs structurally from arb and is modeled separately:
//   * **winner-take-all** — only the FIRST liquidator lands; there is no partial fill,
//     so capture is a single win probability, not a fraction of the edge;
//   * **oracle-gated** — the opportunity only opens when a fresh price crosses the
//     threshold, and stays open for a short window before a bot takes it; capture is
//     tied to our oracle-update + landing latency vs that window;
//   * **heavily contested** — established, well-optimized liquidation bots ⇒ model the
//     win probability much lower than arb's.
// The upside: liquidation profit is fat-tailed — a single large liquidation can dwarf a
// month of arb — so a low capture rate on a large episode can still beat arb.

/// Liquidation-race assumptions.
#[derive(Clone, Copy, Debug)]
pub struct LiqFrictionParams {
    /// Our oracle-update + build + land latency (ms) once the position goes underwater.
    pub our_latency_ms: f64,
    /// Competing liquidation bots racing the same position.
    pub competitors: f64,
    /// How long the opportunity stays open after it appears (ms) before someone takes it.
    pub opportunity_window_ms: f64,
    /// Gas paid when we submit but lose the race (the liquidate reverts — healthy again).
    pub gas_cost_usd: f64,
}

impl Default for LiqFrictionParams {
    fn default() -> Self {
        // Short window (a few checkpoints), several fast competitors, a few cents gas.
        Self {
            our_latency_ms: 700.0,
            competitors: 5.0,
            opportunity_window_ms: 1_500.0,
            gas_cost_usd: 0.05,
        }
    }
}

/// Probability we land the liquidation first: we must both be inside the window
/// (`exp(-latency/window)`) and win the race against the other bots (`1/(1+competitors)`).
#[must_use]
pub fn p_capture_liquidation(p: &LiqFrictionParams) -> f64 {
    let in_window = (-p.our_latency_ms / p.opportunity_window_ms).exp();
    in_window / (1.0 + p.competitors.max(0.0))
}

/// Expected net USD for one liquidation episode. Winner-take-all: capture the whole
/// bonus with `p_capture`, else (we submitted but lost) the on-chain liquidate reverts
/// for gas only. The profit gate caps downside at gas — never principal.
#[must_use]
pub fn adjusted_liquidation(paper_profit_usd: f64, p: &LiqFrictionParams) -> f64 {
    let pc = p_capture_liquidation(p);
    pc * paper_profit_usd - (1.0 - pc) * p.gas_cost_usd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(edge_bps: f64, profit: f64) -> Episode {
        Episode {
            edge_bps,
            paper_profit_usd: profit,
        }
    }

    #[test]
    fn survival_falls_with_latency_and_rises_with_edge() {
        let slow = FrictionParams {
            our_latency_ms: 2_000.0,
            ..Default::default()
        };
        let fast = FrictionParams {
            our_latency_ms: 200.0,
            ..Default::default()
        };
        assert!(p_alive(20.0, &fast) > p_alive(20.0, &slow));
        // a bigger edge survives longer at the same latency
        assert!(p_alive(40.0, &slow) > p_alive(10.0, &slow));
        // bounded in (0,1]
        let a = p_alive(20.0, &fast);
        assert!(a > 0.0 && a <= 1.0);
    }

    #[test]
    fn more_competitors_lowers_win_and_capture() {
        let few = FrictionParams {
            competitors: 1.0,
            ..Default::default()
        };
        let many = FrictionParams {
            competitors: 9.0,
            ..Default::default()
        };
        assert!(p_win(&few) > p_win(&many));
        let e = ep(30.0, 1.0);
        assert!(adjusted_profit(&e, &few) > adjusted_profit(&e, &many));
    }

    #[test]
    fn adjusted_never_exceeds_paper_for_positive_edge() {
        let p = FrictionParams::default();
        let e = ep(30.0, 2.0);
        let adj = adjusted_profit(&e, &p);
        assert!(
            adj <= e.paper_profit_usd,
            "frictions can only reduce expectation"
        );
    }

    #[test]
    fn thin_edge_under_competition_can_go_negative() {
        // A tiny edge that rarely survives, many competitors, real gas → net loss.
        let p = FrictionParams {
            our_latency_ms: 1_500.0,
            competitors: 8.0,
            halflife_ms_per_bp: 100.0,
            gas_cost_usd: 0.03,
        };
        let e = ep(2.0, 0.02); // 2 bp, 2 cents paper
        assert!(adjusted_profit(&e, &p) < 0.0);
    }

    #[test]
    fn aggregate_capture_ratio_in_unit_range() {
        let p = FrictionParams::default();
        let eps = [ep(29.4, 2.14), ep(10.0, 0.10), ep(5.0, 0.06)];
        let agg = aggregate(&eps, &p);
        assert_eq!(agg.episodes, 3);
        assert!(agg.adjusted_usd < agg.paper_usd);
        assert!(agg.capture_ratio > 0.0 && agg.capture_ratio < 1.0);
    }

    #[test]
    fn liquidation_capture_is_lower_than_arb_and_drops_with_competition() {
        let few = LiqFrictionParams {
            competitors: 1.0,
            ..Default::default()
        };
        let many = LiqFrictionParams {
            competitors: 12.0,
            ..Default::default()
        };
        assert!(p_capture_liquidation(&few) > p_capture_liquidation(&many));
        // winner-take-all ⇒ capture well below arb's 1/(1+c) at the same competitor count
        let liq = p_capture_liquidation(&LiqFrictionParams {
            competitors: 3.0,
            ..Default::default()
        });
        let arb = p_win(&FrictionParams {
            competitors: 3.0,
            ..Default::default()
        });
        assert!(liq < arb);
        assert!((0.0..=1.0).contains(&p_capture_liquidation(&few)));
    }

    #[test]
    fn fat_tail_liquidation_positive_even_at_low_capture() {
        // A big liquidation ($500 bonus) is net-positive even when we rarely win.
        let p = LiqFrictionParams {
            our_latency_ms: 1_000.0,
            competitors: 10.0,
            ..Default::default()
        };
        assert!(p_capture_liquidation(&p) < 0.15);
        assert!(adjusted_liquidation(500.0, &p) > 0.0);
        // A tiny liquidation is a net loss after gas-on-miss.
        assert!(adjusted_liquidation(0.10, &p) < 0.0);
    }
}
