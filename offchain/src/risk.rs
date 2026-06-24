//! Operational risk controls for the submit path: a pool blacklist, a max-daily-loss
//! guard, a kill switch, and realized-vs-predicted accounting. The executor consults
//! a [`RiskGuard`] before every submit and records the outcome after, emitting a
//! structured decision log.
//!
//! This is a backstop *in addition to* the on-chain gates — it never relaxes them. It
//! exists to stop the bot cheaply (off-chain) when something is systematically wrong:
//! a known-broken pool (the report's Momentum USDB/USDC that quotes ~0), a run of
//! losses, or a manual halt.

use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct RiskConfig {
    /// Halt submitting once cumulative realized P&L for the day drops below
    /// `-max_daily_loss_usd`. 0 disables the check.
    pub max_daily_loss_usd: f64,
    /// Hard manual halt — when true, nothing is ever submitted.
    pub kill_switch: bool,
    /// Pool object ids that must never be routed through (e.g. broken/empty pools).
    pub blacklist: HashSet<String>,
}

impl RiskConfig {
    #[must_use]
    pub fn new(
        max_daily_loss_usd: f64,
        kill_switch: bool,
        blacklist: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            max_daily_loss_usd,
            kill_switch,
            blacklist: blacklist.into_iter().collect(),
        }
    }
}

/// The decision for one candidate, with a reason for the structured log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Submit,
    Skip(&'static str),
}

/// Tracks running P&L + accounting across the session/day.
#[derive(Clone, Debug)]
pub struct RiskGuard {
    cfg: RiskConfig,
    realized_usd: f64,
    predicted_usd: f64,
    submits: u64,
    skips: u64,
}

impl RiskGuard {
    #[must_use]
    pub fn new(cfg: RiskConfig) -> Self {
        Self {
            cfg,
            realized_usd: 0.0,
            predicted_usd: 0.0,
            submits: 0,
            skips: 0,
        }
    }

    /// True if no pool on the route is blacklisted.
    #[must_use]
    pub fn route_allowed(&self, pool_ids: &[&str]) -> bool {
        !pool_ids.iter().any(|id| self.cfg.blacklist.contains(*id))
    }

    /// Decide whether to submit. Order matters: hard halts first, then economics.
    /// `predicted_net_usd` is the dry-run-gated expected net (after gas + flash fee).
    #[must_use]
    pub fn should_submit(&self, predicted_net_usd: f64, pool_ids: &[&str]) -> Decision {
        if self.cfg.kill_switch {
            return Decision::Skip("kill_switch");
        }
        if self.cfg.max_daily_loss_usd > 0.0 && self.realized_usd <= -self.cfg.max_daily_loss_usd {
            return Decision::Skip("daily_loss_limit");
        }
        if !self.route_allowed(pool_ids) {
            return Decision::Skip("blacklisted_pool");
        }
        if predicted_net_usd <= 0.0 {
            return Decision::Skip("not_profitable");
        }
        Decision::Submit
    }

    /// Record that we submitted with this predicted net (for realized-vs-predicted).
    pub fn record_submit(&mut self, predicted_net_usd: f64) {
        self.predicted_usd += predicted_net_usd;
        self.submits += 1;
    }

    pub fn record_skip(&mut self) {
        self.skips += 1;
    }

    /// Record the realized on-chain net (from the landed tx's balance changes − gas;
    /// negative for a reverted/lost race that still cost gas).
    pub fn record_realized(&mut self, realized_net_usd: f64) {
        self.realized_usd += realized_net_usd;
    }

    #[must_use]
    pub fn daily_pnl_usd(&self) -> f64 {
        self.realized_usd
    }

    /// Realized / predicted — the key health metric. < 1 means we're capturing less
    /// than the dry-run promised (latency/competition leakage); ~0 or negative is a
    /// stop signal.
    #[must_use]
    pub fn realized_vs_predicted(&self) -> Option<f64> {
        if self.predicted_usd.abs() < f64::EPSILON {
            None
        } else {
            Some(self.realized_usd / self.predicted_usd)
        }
    }

    #[must_use]
    pub fn submits(&self) -> u64 {
        self.submits
    }
    #[must_use]
    pub fn skips(&self) -> u64 {
        self.skips
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(max_loss: f64, kill: bool, blacklist: &[&str]) -> RiskGuard {
        RiskGuard::new(RiskConfig::new(
            max_loss,
            kill,
            blacklist.iter().map(|s| s.to_string()),
        ))
    }

    #[test]
    fn kill_switch_blocks_everything() {
        let g = guard(0.0, true, &[]);
        assert_eq!(
            g.should_submit(100.0, &["0xA"]),
            Decision::Skip("kill_switch")
        );
    }

    #[test]
    fn blacklisted_pool_is_skipped() {
        let g = guard(0.0, false, &["0xBAD"]);
        assert_eq!(
            g.should_submit(5.0, &["0xA", "0xBAD"]),
            Decision::Skip("blacklisted_pool")
        );
        assert_eq!(g.should_submit(5.0, &["0xA", "0xB"]), Decision::Submit);
        assert!(!g.route_allowed(&["0xBAD"]));
    }

    #[test]
    fn unprofitable_is_skipped() {
        let g = guard(0.0, false, &[]);
        assert_eq!(
            g.should_submit(0.0, &["0xA"]),
            Decision::Skip("not_profitable")
        );
        assert_eq!(
            g.should_submit(-1.0, &["0xA"]),
            Decision::Skip("not_profitable")
        );
    }

    #[test]
    fn daily_loss_limit_halts_after_losses() {
        let mut g = guard(10.0, false, &[]);
        assert_eq!(g.should_submit(1.0, &["0xA"]), Decision::Submit);
        g.record_realized(-11.0); // breach the $10 daily loss cap
        assert_eq!(
            g.should_submit(1.0, &["0xA"]),
            Decision::Skip("daily_loss_limit")
        );
        assert!((g.daily_pnl_usd() - (-11.0)).abs() < 1e-9);
    }

    #[test]
    fn realized_vs_predicted_tracks_leakage() {
        let mut g = guard(0.0, false, &[]);
        assert_eq!(g.realized_vs_predicted(), None); // nothing submitted yet
        g.record_submit(1.00);
        g.record_realized(0.40); // captured 40% of the dry-run promise
        assert_eq!(g.submits(), 1);
        assert!((g.realized_vs_predicted().unwrap() - 0.40).abs() < 1e-9);
    }
}
