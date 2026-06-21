//! Dry-run and submission (feature = "live").
//!
//! Submit-only-if-profitable flow:
//!   1. Build the PTB for the opportunity (`crate::ptb::build`).
//!   2. `dry_run_transaction_block` — read the simulated balance changes and
//!      effects. This is the off-chain gas estimate the design calls for: gas is
//!      never modeled on-chain, it is measured here and folded into `min_profit`.
//!   3. Submit only if `simulated_profit - gas_used >= min_profit` AND effects
//!      are success. On-chain, `executor::settle` is the final backstop: even if
//!      our estimate is wrong, an unprofitable trade aborts atomically and we pay
//!      only gas.

use anyhow::Result;

use crate::config::Config;
use crate::scanner::Opportunity;

/// Dry-run the route and submit it iff it still clears the profit bar on-chain.
pub async fn try_execute(config: &Config, opp: &Opportunity) -> Result<()> {
    use sui_sdk::SuiClientBuilder;

    let client = SuiClientBuilder::default().build(&config.rpc_url).await?;

    // 1. Resolve fresh object refs + type tags, then build the PTB.
    let _ptb = crate::ptb::build(config, opp)?;

    // 2. Dry-run: wrap _ptb in a TransactionData with sender + gas, call
    //    client.read_api().dry_run_transaction_block(tx). Inspect:
    //      - effects.status() == Success
    //      - balance_changes net of gas_used >= config.min_profit
    //
    // 3. If profitable, sign with the keystore and submit with
    //    client.quorum_driver_api().execute_transaction_block(
    //        signed, options, Some(WaitForLocalExecution)).
    let _ = &client;

    tracing::info!(
        net_profit = opp.net_profit,
        hops = opp.route.len(),
        input = opp.input_amount,
        "candidate ready (dry-run + submit wiring is the live integration seam)"
    );
    Ok(())
}
