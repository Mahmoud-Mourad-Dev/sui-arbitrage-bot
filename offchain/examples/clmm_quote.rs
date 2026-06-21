//! Batch CLMM quoter: reads one JSON scenario per stdin line, prints the engine's
//! exact-in output per line (or `none`). Used by the Cetus parity harness to drive
//! the production `clmm` engine on real on-chain pool state.
//!
//! Scenario JSON (u128 values as strings so they survive JSON):
//!   {"sqrt_price":"...", "liquidity":"...", "fee_pips":2500, "amount":1000000,
//!    "a_to_b":true, "ticks":[["<sqrt_price>","<liquidity_net>"], ...]}

use std::io::{self, BufRead, Write};

use arb_scanner::clmm::{self, ClmmState, TickBoundary};
use serde::Deserialize;

#[derive(Deserialize)]
struct Scenario {
    sqrt_price: String,
    liquidity: String,
    fee_pips: u64,
    amount: u64,
    a_to_b: bool,
    #[serde(default)]
    ticks: Vec<(String, String)>,
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let s: Scenario = serde_json::from_str(&line).expect("bad scenario json");
        let ticks: Vec<TickBoundary> = s
            .ticks
            .iter()
            .map(|(sp, net)| TickBoundary {
                sqrt_price: sp.parse().expect("sqrt_price"),
                liquidity_net: net.parse().expect("liquidity_net"),
            })
            .collect();
        let state = ClmmState {
            sqrt_price: s.sqrt_price.parse().expect("sqrt_price"),
            liquidity: s.liquidity.parse().expect("liquidity"),
            fee_pips: s.fee_pips,
            ticks,
        };
        match clmm::quote_exact_in(&state, s.amount, s.a_to_b) {
            Some(o) => writeln!(out, "{o}").unwrap(),
            None => writeln!(out, "none").unwrap(),
        }
    }
}
