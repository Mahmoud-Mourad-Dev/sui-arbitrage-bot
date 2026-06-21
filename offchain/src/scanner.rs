//! Arbitrage detection: find profitable cycles in the cached pool graph and size
//! the trade.
//!
//! Model: each pool is two directed edges (a->b, b->a). A cyclic arbitrage starts
//! and ends at `base_token` (so the executor's profit gate compares like-for-like
//! `Coin<Base>`). We enumerate simple cycles up to `max_hops`, simulate each over
//! a set of candidate input sizes using the exact `amm` math, subtract a gas
//! estimate, and return the most profitable opportunity that clears `min_profit`.

use std::collections::{HashMap, HashSet};

use crate::amm;
use crate::types::{Dex, PoolId, PoolState, TokenId};

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

/// A profitable (simulated) route, ready to be turned into a PTB.
#[derive(Clone, Debug)]
pub struct Opportunity {
    pub route: Vec<Hop>,
    pub input_amount: u64,
    pub output_amount: u64,
    pub gross_profit: u64,
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
        let net_profit = gross_profit.saturating_sub(params.gas_cost);
        if net_profit < params.min_profit {
            continue;
        }
        if best.as_ref().is_none_or(|b| net_profit > b.net_profit) {
            best = Some(Opportunity {
                route: route.to_vec(),
                input_amount: input,
                output_amount: output,
                gross_profit,
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

/// Run `input` through every hop using exact AMM math. `None` if any hop fails.
fn simulate(by_id: &HashMap<&str, &PoolState>, route: &[Hop], input: u64) -> Option<u64> {
    let mut amount = input;
    for hop in route {
        let pool = by_id.get(hop.pool_id.as_str())?;
        let (reserve_in, reserve_out) = pool.reserves_from(&hop.token_in)?;
        amount = amm::get_amount_out(amount, reserve_in, reserve_out, pool.fee_bps)?;
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
        PoolState {
            id: id.into(),
            dex: Dex::AmmV2,
            token_a: a.into(),
            token_b: b.into(),
            reserve_a: ra,
            reserve_b: rb,
            fee_bps: 30,
        }
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
            min_profit: 1,
        };
        assert!(find_best(&pools, &params).is_none());
    }
}
