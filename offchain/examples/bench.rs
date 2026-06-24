//! Phase 7 stress test: 100 consecutive scans over the on-chain pool graph,
//! measuring scan latency, single-route simulation latency, and PTB-plan build
//! time using the production scanner. (On-chain execution time is measured
//! separately via real testnet submissions.)

use std::time::Instant;

use arb_scanner::scanner::{self, Opportunity, ScanParams};
use arb_scanner::types::{Dex, PoolState};

const N: usize = 100;

fn pools() -> Vec<PoolState> {
    vec![
        mk("AB", "A", "B", 1_000_000_000_000, 1_000_000_000_000),
        mk("BC", "B", "C", 1_000_000_000_000, 1_000_000_000_000),
        mk("CA", "C", "A", 1_000_000_000_000, 2_000_000_000_000),
    ]
}

fn mk(id: &str, a: &str, b: &str, ra: u64, rb: u64) -> PoolState {
    PoolState::v2(id, Dex::AmmV2, a, b, ra, rb, 30)
}

fn params() -> ScanParams {
    ScanParams {
        base_token: "A".into(),
        max_hops: 3,
        candidate_inputs: vec![
            100_000_000,
            500_000_000,
            1_000_000_000,
            5_000_000_000,
            10_000_000_000,
            50_000_000_000,
            100_000_000_000,
        ],
        gas_cost: 8_000_000,
        flash_fee_bps: 0,
        min_profit: 1,
    }
}

/// Offline PTB plan: the ordered move-calls the live builder emits (begin → per-hop
/// adapter swap → settle). Mirrors `ptb::build` without needing the SDK.
fn build_ptb_plan(opp: &Opportunity, pkg: &str, base: &str) -> Vec<String> {
    let mut cmds = Vec::with_capacity(opp.route.len() + 2);
    cmds.push(format!("{pkg}::executor::begin<{base}>"));
    for h in &opp.route {
        let module = match h.dex {
            Dex::AmmV2 => "amm_v2_adapter",
            Dex::Cetus => "cetus_adapter",
            Dex::Turbos => "turbos_adapter",
        };
        let func = if h.a_to_b {
            "swap_exact_in_a_to_b"
        } else {
            "swap_exact_in_b_to_a"
        };
        cmds.push(format!(
            "{pkg}::{module}::{func}<{},{}>",
            h.token_in, h.token_out
        ));
    }
    cmds.push(format!("{pkg}::executor::settle<{base}>"));
    cmds
}

fn report(label: &str, mut ns: Vec<u128>) {
    ns.sort_unstable();
    let avg = ns.iter().sum::<u128>() / ns.len() as u128;
    let p50 = ns[ns.len() / 2];
    let p99 = ns[(ns.len() * 99 / 100).min(ns.len() - 1)];
    let max = *ns.last().unwrap();
    println!(
        "{label:<22} avg={:>8.3}µs  p50={:>8.3}µs  p99={:>8.3}µs  max={:>8.3}µs",
        avg as f64 / 1000.0,
        p50 as f64 / 1000.0,
        p99 as f64 / 1000.0,
        max as f64 / 1000.0,
    );
}

fn main() {
    let pools = pools();
    let params = params();
    let opp = scanner::find_best(&pools, &params).expect("opportunity");

    let mut scan = Vec::with_capacity(N);
    let mut sim = Vec::with_capacity(N);
    let mut build = Vec::with_capacity(N);

    for _ in 0..N {
        let t = Instant::now();
        let _ = scanner::find_best(&pools, &params);
        scan.push(t.elapsed().as_nanos());

        let t = Instant::now();
        let _ = scanner::simulate_route(&pools, &opp.route, opp.input_amount);
        sim.push(t.elapsed().as_nanos());

        let t = Instant::now();
        let _ = build_ptb_plan(&opp, "0x5de0", "0x60e4::coin_a::COIN_A");
        build.push(t.elapsed().as_nanos());
    }

    println!("Phase 7 — {N} consecutive scans (3 pools, max_hops=3)\n");
    report("scan latency", scan);
    report("simulation latency", sim);
    report("PTB build (plan)", build);
}
