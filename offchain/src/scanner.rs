//! Arbitrage detection: find profitable cycles in the cached pool graph and size
//! the trade.
//!
//! Model: each pool is two directed edges (a->b, b->a). A cyclic arbitrage starts
//! and ends at `base_token` (so the executor's profit gate compares like-for-like
//! `Coin<Base>`). We enumerate simple cycles up to `max_hops`, simulate each over
//! a set of candidate input sizes — pricing each hop with its pool's native model
//! (V2 → `amm`, CLMM → `clmm`) — subtract a gas estimate, and return the most
//! profitable opportunity that clears `min_profit`.
//!
//! This is **funnel stage 1** (fast, in-process; see docs/consolidation-plan.md).
//! The chosen candidate must then be re-priced authoritatively via [`reprice_route`]
//! with a venue quoter before it is acted on — engine estimates over-detect.

use std::collections::{HashMap, HashSet};

use crate::types::{Dex, PoolId, PoolKind, PoolState, TokenId};
use crate::{amm, clmm};

/// One swap in a route.
#[derive(Clone, Debug)]
pub struct Hop {
    pub pool_id: PoolId,
    pub dex: Dex,
    pub token_in: TokenId,
    pub token_out: TokenId,
    /// True if `token_in` is the pool's `token_a` (so the adapter call is a->b).
    pub a_to_b: bool,
}

/// Which opportunity source produced this (arb scanner, liquidation, backrun). The
/// pipeline (dry-run → risk → submit) treats them uniformly; only frictions/race
/// modeling and PTB assembly differ per kind. Liquidation/backrun payloads are added
/// in later phases — this marker keeps the profit semantics + pipeline identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OppKind {
    #[default]
    Arb,
    Liquidation,
    Backrun,
}

/// Target lending protocol for a liquidation leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Scallop,
    Suilend,
    Navi,
}

/// A protocol-liquidate leg, prepended before the swap-back hops when
/// `kind == OppKind::Liquidation`. Built from the authoritative on-chain sizing read.
#[derive(Clone, Debug)]
pub struct LiquidationLeg {
    pub protocol: Protocol,
    pub obligation_id: String,
    pub debt_type: TokenId,
    pub collateral_type: TokenId,
    /// Repay amount (raw debt units) — from the protocol's `calculate_liquidation_amounts`.
    pub repay_amount: u64,
    /// Extra shared object ids the liquidate call needs (market, registry, x_oracle, …),
    /// resolved fresh on-chain by the live PTB assembler.
    pub extra_object_ids: Vec<String>,
}

/// A profitable (simulated) route, ready to be turned into a PTB. `input_amount` and
/// `output_amount` are always in the **base asset** (start == end), so the executor's
/// profit gate compares like-for-like regardless of `kind`.
#[derive(Clone, Debug)]
pub struct Opportunity {
    /// Source that produced this opportunity.
    pub kind: OppKind,
    /// Set when `kind == Liquidation`: the protocol-liquidate leg run before the swaps.
    pub liquidation: Option<LiquidationLeg>,
    pub route: Vec<Hop>,
    pub input_amount: u64,
    pub output_amount: u64,
    pub gross_profit: u64,
    /// Flash-loan fee charged on `input_amount` (0 when trading owned capital).
    pub flash_fee: u64,
    /// gross_profit − gas_cost − flash_fee.
    pub net_profit: u64,
}

/// Tuning knobs for a scan.
#[derive(Clone, Debug)]
pub struct ScanParams {
    pub base_token: TokenId,
    pub max_hops: usize,
    /// Candidate input sizes (in base-token MIST) to try per cycle.
    pub candidate_inputs: Vec<u64>,
    /// Estimated gas cost of the whole PTB, charged against gross profit.
    pub gas_cost: u64,
    /// Flash-loan fee in bps charged on the input/loan size. 0 = owned capital
    /// (no flash loan). When > 0, `input_amount` is the borrowed amount and the
    /// fee is subtracted from profit so routes that only clear without the loan
    /// fee are rejected.
    pub flash_fee_bps: u64,
    /// Minimum net profit to report an opportunity.
    pub min_profit: u64,
}

#[derive(Clone)]
struct Edge {
    pool_id: PoolId,
    dex: Dex,
    from: TokenId,
    to: TokenId,
    a_to_b: bool,
}

/// Scan all pools and return the single best opportunity, if any clears the bar.
#[must_use]
pub fn find_best(pools: &[PoolState], params: &ScanParams) -> Option<Opportunity> {
    let by_id: HashMap<&str, &PoolState> = pools.iter().map(|p| (p.id.as_str(), p)).collect();
    let adjacency = build_adjacency(pools);
    let cycles = find_cycles(&adjacency, &params.base_token, params.max_hops);

    let mut best: Option<Opportunity> = None;
    for route in cycles {
        if let Some(opp) = size_route(&by_id, &route, params) {
            if best.as_ref().is_none_or(|b| opp.net_profit > b.net_profit) {
                best = Some(opp);
            }
        }
    }
    best
}

/// Best input size for a fixed route, if it clears `min_profit` net of gas.
fn size_route(
    by_id: &HashMap<&str, &PoolState>,
    route: &[Hop],
    params: &ScanParams,
) -> Option<Opportunity> {
    let mut best: Option<Opportunity> = None;
    for &input in &params.candidate_inputs {
        let Some(output) = simulate(by_id, route, input) else {
            continue;
        };
        if output <= input {
            continue;
        }
        let gross_profit = output - input;
        // Borrowed capital: you owe input + flash_fee, so profit nets the fee too.
        let flash_fee = if params.flash_fee_bps > 0 {
            crate::flashloan::quote_fee_bps(input, params.flash_fee_bps)
        } else {
            0
        };
        let net_profit = gross_profit
            .saturating_sub(params.gas_cost)
            .saturating_sub(flash_fee);
        if net_profit < params.min_profit {
            continue;
        }
        if best.as_ref().is_none_or(|b| net_profit > b.net_profit) {
            best = Some(Opportunity {
                kind: OppKind::Arb,
                liquidation: None,
                route: route.to_vec(),
                input_amount: input,
                output_amount: output,
                gross_profit,
                flash_fee,
                net_profit,
            });
        }
    }
    best
}

/// Simulate a fixed route for a given input using exact AMM math. Public wrapper
/// over the internal per-hop simulation (used by validation + benchmarks).
#[must_use]
pub fn simulate_route(pools: &[PoolState], route: &[Hop], input: u64) -> Option<u64> {
    let by_id: HashMap<&str, &PoolState> = pools.iter().map(|p| (p.id.as_str(), p)).collect();
    simulate(&by_id, route, input)
}

/// Run `input` through every hop with the engine (funnel stage 1). `None` if any
/// hop can't be quoted.
fn simulate(by_id: &HashMap<&str, &PoolState>, route: &[Hop], input: u64) -> Option<u64> {
    let mut amount = input;
    for hop in route {
        let pool = by_id.get(hop.pool_id.as_str())?;
        amount = quote_hop(pool, hop, amount)?;
    }
    Some(amount)
}

/// Price one hop using the pool's native model: V2 → exact `amm` math (bit-identical
/// to `math.move`), CLMM → the `clmm` tick-crossing engine. `hop.a_to_b` is true when
/// `token_in == token_a`, which is also the engine's token0→token1 direction.
fn quote_hop(pool: &PoolState, hop: &Hop, amount_in: u64) -> Option<u64> {
    match &pool.kind {
        PoolKind::V2 {
            reserve_a,
            reserve_b,
            fee_bps,
        } => {
            let (reserve_in, reserve_out) = if hop.a_to_b {
                (*reserve_a, *reserve_b)
            } else {
                (*reserve_b, *reserve_a)
            };
            amm::get_amount_out(amount_in, reserve_in, reserve_out, *fee_bps)
        }
        PoolKind::Clmm(state) => clmm::quote_exact_in(state, amount_in, hop.a_to_b),
    }
}

/// Stage-2 authoritative quoting seam (see docs/consolidation-plan.md, Decision 1).
/// The engine ([`EngineQuoter`]) is the default/offline implementation; the live path
/// supplies a `devInspect` quoter that calls each venue's own on-chain quoter, and
/// the best candidate is re-priced through it before it is treated as an opportunity.
pub trait Quoter {
    fn quote(&self, pool: &PoolState, hop: &Hop, amount_in: u64) -> Option<u64>;
}

/// Default quoter: prices with the in-process engines (`amm` / `clmm`).
pub struct EngineQuoter;

impl Quoter for EngineQuoter {
    fn quote(&self, pool: &PoolState, hop: &Hop, amount_in: u64) -> Option<u64> {
        quote_hop(pool, hop, amount_in)
    }
}

/// Re-price a fixed route end-to-end with an authoritative quoter (funnel stage 2).
/// Returns the final output, or `None` if any hop can't be quoted.
#[must_use]
pub fn reprice_route<Q: Quoter>(
    pools: &[PoolState],
    route: &[Hop],
    input: u64,
    quoter: &Q,
) -> Option<u64> {
    let by_id: HashMap<&str, &PoolState> = pools.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut amount = input;
    for hop in route {
        let pool = by_id.get(hop.pool_id.as_str())?;
        amount = quoter.quote(pool, hop, amount)?;
    }
    Some(amount)
}

fn build_adjacency(pools: &[PoolState]) -> HashMap<TokenId, Vec<Edge>> {
    let mut adj: HashMap<TokenId, Vec<Edge>> = HashMap::new();
    for p in pools {
        adj.entry(p.token_a.clone()).or_default().push(Edge {
            pool_id: p.id.clone(),
            dex: p.dex,
            from: p.token_a.clone(),
            to: p.token_b.clone(),
            a_to_b: true,
        });
        adj.entry(p.token_b.clone()).or_default().push(Edge {
            pool_id: p.id.clone(),
            dex: p.dex,
            from: p.token_b.clone(),
            to: p.token_a.clone(),
            a_to_b: false,
        });
    }
    adj
}

/// Enumerate simple cycles base -> ... -> base of length 2..=max_hops.
fn find_cycles(
    adj: &HashMap<TokenId, Vec<Edge>>,
    base: &TokenId,
    max_hops: usize,
) -> Vec<Vec<Hop>> {
    let mut cycles = Vec::new();
    let mut path: Vec<Hop> = Vec::new();
    let mut visited: HashSet<TokenId> = HashSet::new();
    visited.insert(base.clone());
    dfs(
        adj,
        base,
        base,
        max_hops,
        &mut visited,
        &mut path,
        &mut cycles,
    );
    cycles
}

fn dfs(
    adj: &HashMap<TokenId, Vec<Edge>>,
    base: &TokenId,
    current: &TokenId,
    max_hops: usize,
    visited: &mut HashSet<TokenId>,
    path: &mut Vec<Hop>,
    cycles: &mut Vec<Vec<Hop>>,
) {
    let Some(edges) = adj.get(current) else {
        return;
    };
    for edge in edges {
        let new_len = path.len() + 1;
        if &edge.to == base {
            if (2..=max_hops).contains(&new_len) {
                let mut full = path.clone();
                full.push(hop_of(edge));
                cycles.push(full);
            }
            continue; // never route *through* the base token
        }
        if !visited.contains(&edge.to) && new_len < max_hops {
            visited.insert(edge.to.clone());
            path.push(hop_of(edge));
            dfs(adj, base, &edge.to, max_hops, visited, path, cycles);
            path.pop();
            visited.remove(&edge.to);
        }
    }
}

fn hop_of(edge: &Edge) -> Hop {
    Hop {
        pool_id: edge.pool_id.clone(),
        dex: edge.dex,
        token_in: edge.from.clone(),
        token_out: edge.to.clone(),
        a_to_b: edge.a_to_b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(id: &str, a: &str, b: &str, ra: u64, rb: u64) -> PoolState {
        PoolState::v2(id, Dex::AmmV2, a, b, ra, rb, 30)
    }

    /// A single-range CLMM pool — the proven V2-equivalent (clmm.rs bridge):
    /// `sqrt_price = √(rb/ra)·2^64`, `liquidity = √(ra·rb)`. 0.30% fee (3000 pips).
    fn clmm_pool(id: &str, a: &str, b: &str, sqrt_price: u128, liquidity: u128) -> PoolState {
        PoolState::clmm(
            id,
            Dex::Cetus,
            a,
            b,
            clmm::single_range(sqrt_price, liquidity, 3000),
        )
    }

    #[test]
    fn finds_profitable_triangle() {
        // Same dislocation as the Move arb_route_tests: C is worth ~2 A.
        let pools = vec![
            pool("0xAB", "A", "B", 1_000, 1_000),
            pool("0xBC", "B", "C", 1_000, 1_000),
            pool("0xCA", "C", "A", 1_000, 2_000),
        ];
        let params = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1, 2, 5, 10, 20, 50, 100],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        };
        let opp = find_best(&pools, &params).expect("should find an opportunity");
        assert_eq!(opp.route.len(), 3);
        assert_eq!(opp.route[0].token_in, "A");
        assert_eq!(opp.route.last().unwrap().token_out, "A");
        assert!(opp.net_profit > 0);
        assert!(opp.output_amount > opp.input_amount);
    }

    #[test]
    fn no_opportunity_when_balanced() {
        // All 1:1, fee on every hop -> round trip always loses.
        let pools = vec![
            pool("0xAB", "A", "B", 1_000_000, 1_000_000),
            pool("0xBC", "B", "C", 1_000_000, 1_000_000),
            pool("0xCA", "C", "A", 1_000_000, 1_000_000),
        ];
        let params = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1, 10, 100, 1_000, 10_000],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        };
        assert!(find_best(&pools, &params).is_none());
    }

    #[test]
    fn gas_cost_can_erase_thin_profit() {
        let pools = vec![
            pool("0xAB", "A", "B", 1_000, 1_000),
            pool("0xBC", "B", "C", 1_000, 1_000),
            pool("0xCA", "C", "A", 1_000, 2_000),
        ];
        let params = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1, 2, 5, 10],
            gas_cost: 1_000_000, // dwarfs any micro-spread profit here
            flash_fee_bps: 0,
            min_profit: 1,
        };
        assert!(find_best(&pools, &params).is_none());
    }

    #[test]
    fn flash_fee_is_subtracted_and_can_reject_routes() {
        // The C~2A triangle: ~5 profit on input 10 with no fee.
        let pools = vec![
            pool("0xAB", "A", "B", 1_000, 1_000),
            pool("0xBC", "B", "C", 1_000, 1_000),
            pool("0xCA", "C", "A", 1_000, 2_000),
        ];
        let base = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![10],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        };
        let no_fee = find_best(&pools, &base).expect("profitable without fee");
        assert_eq!(no_fee.flash_fee, 0);

        // With a flash fee, the same route nets less by exactly the fee.
        let with_fee = find_best(
            &pools,
            &ScanParams {
                flash_fee_bps: 30,
                ..base.clone()
            },
        )
        .expect("still profitable after a small fee");
        assert_eq!(with_fee.flash_fee, crate::flashloan::quote_fee_bps(10, 30));
        assert_eq!(with_fee.net_profit, no_fee.net_profit - with_fee.flash_fee);

        // A punitive fee (50%) makes the thin micro-spread unprofitable -> rejected.
        let killed = find_best(
            &pools,
            &ScanParams {
                flash_fee_bps: 5_000,
                ..base
            },
        );
        assert!(killed.is_none());
    }

    #[test]
    fn clmm_triangle_matches_v2_within_tolerance_and_is_profitable() {
        // The same C≈4A dislocation expressed two ways: as V2 reserves, and as the
        // equivalent single-range CLMM (clmm.rs proves these price identically up to
        // integer rounding). Same pool ids + structure ⇒ find_best picks the same
        // route, so we can diff the engine's CLMM output against the V2 closed form
        // (our offline authoritative reference) hop-for-hop.
        let q = clmm::Q64;
        let v2 = vec![
            pool("0xAB", "A", "B", 1_000_000_000, 1_000_000_000),
            pool("0xBC", "B", "C", 1_000_000_000, 1_000_000_000),
            pool("0xCA", "C", "A", 1_000_000_000, 4_000_000_000),
        ];
        let clmm_pools = vec![
            clmm_pool("0xAB", "A", "B", q, 1_000_000_000), // 1:1
            clmm_pool("0xBC", "B", "C", q, 1_000_000_000), // 1:1
            clmm_pool("0xCA", "C", "A", 2 * q, 2_000_000_000), // 1:4 (price 4)
        ];
        let params = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1_000_000], // tiny vs depth ⇒ stays in one range
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        };
        let v2_opp = find_best(&v2, &params).expect("v2 triangle profitable");
        let clmm_opp = find_best(&clmm_pools, &params).expect("clmm triangle profitable");

        assert!(clmm_opp.net_profit > 0);
        // same route structure (ids line up)
        let v2_ids: Vec<_> = v2_opp.route.iter().map(|h| h.pool_id.as_str()).collect();
        let cl_ids: Vec<_> = clmm_opp.route.iter().map(|h| h.pool_id.as_str()).collect();
        assert_eq!(v2_ids, cl_ids);
        // engine CLMM output matches the V2 closed form within the documented tolerance
        // (≤ a few units per hop accumulates to a tiny fraction of the output).
        let tol = v2_opp.output_amount / 10_000 + 24; // 0.01% + 3×rounding
        assert!(
            v2_opp.output_amount.abs_diff(clmm_opp.output_amount) <= tol,
            "v2 {} vs clmm {} exceeds tolerance {tol}",
            v2_opp.output_amount,
            clmm_opp.output_amount
        );
    }

    #[test]
    fn clmm_balanced_market_yields_no_opportunity() {
        // All pools 1:1 (sqrt_price = 2^64), equal depth, fee on every hop ⇒ any
        // round trip loses to fees.
        let q = clmm::Q64;
        let pools = vec![
            clmm_pool("0xAB", "A", "B", q, 1_000_000_000_000),
            clmm_pool("0xBC", "B", "C", q, 1_000_000_000_000),
            clmm_pool("0xCA", "C", "A", q, 1_000_000_000_000),
        ];
        let params = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1_000_000, 100_000_000, 10_000_000_000],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        };
        assert!(find_best(&pools, &params).is_none());
    }

    #[test]
    fn engine_quoter_reprice_matches_stage1_simulate() {
        // Stage-2 seam consistency: re-pricing a route with the EngineQuoter equals
        // the stage-1 simulate over the same engine (the live devInspect quoter swaps
        // in here later).
        let q = clmm::Q64;
        let pools = vec![
            clmm_pool("0xAB", "A", "B", q, 1_000_000_000),
            clmm_pool("0xBC", "B", "C", q, 1_000_000_000),
            clmm_pool("0xCA", "C", "A", 2 * q, 2_000_000_000),
        ];
        let params = ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1_000_000],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        };
        let opp = find_best(&pools, &params).expect("profitable");
        let repriced = reprice_route(&pools, &opp.route, opp.input_amount, &EngineQuoter).unwrap();
        assert_eq!(repriced, opp.output_amount);
    }
}
