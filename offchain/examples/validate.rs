//! Testnet validation helper: feed the REAL on-chain pool reserves into the
//! production scanner and print its prediction. Used by the validation process to
//! compare off-chain simulation against on-chain execution.
//!
//! Reserves below are the confirmed testnet values (see docs/deployment-testnet.md).

use arb_scanner::scanner::{self, ScanParams};
use arb_scanner::types::{Dex, PoolState};

fn main() {
    let pools = vec![
        PoolState::v2(
            "AB",
            Dex::AmmV2,
            "A",
            "B",
            1_000_000_000_000,
            1_000_000_000_000,
            30,
        ),
        PoolState::v2(
            "BC",
            Dex::AmmV2,
            "B",
            "C",
            1_000_000_000_000,
            1_000_000_000_000,
            30,
        ),
        PoolState::v2(
            "CA",
            Dex::AmmV2,
            "C",
            "A",
            1_000_000_000_000,
            2_000_000_000_000,
            30,
        ),
    ];

    let gas_cost: u64 = std::env::var("GAS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8_000_000);
    let params = ScanParams {
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
        gas_cost,
        flash_fee_bps: 0,
        min_profit: 1,
    };

    match scanner::find_best(&pools, &params) {
        Some(o) => {
            println!("ROUTE_LEN={}", o.route.len());
            for h in &o.route {
                println!("HOP={}->{} a_to_b={}", h.token_in, h.token_out, h.a_to_b);
            }
            println!("INPUT={}", o.input_amount);
            println!("EXPECTED_OUTPUT={}", o.output_amount);
            println!("GROSS_PROFIT={}", o.gross_profit);
            println!("GAS_COST_EST={}", gas_cost);
            println!("NET_PROFIT={}", o.net_profit);
        }
        None => println!("NO_OPP"),
    }
}
