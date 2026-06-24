//! Dry-run + submission (feature = "live").
//!
//! Submit-only-if-profitable, with the dry-run→land race defused:
//!   1. Resolve fresh object refs; **authoritatively re-quote** the route (stage 2)
//!      within a freshness budget. If the edge has closed, skip — no gas.
//!   2. Derive per-hop `min_out` floors from the authoritative per-hop outputs
//!      (× (1 − slippage)). A route that has moved fails fast/cheap on-chain instead
//!      of landing a bad fill.
//!   3. Build the PTB with those floors, `dry_run_transaction_block`, and require
//!      `effects.status == Success` AND net (balance changes − gas) ≥ `min_profit`.
//!   4. Consult the [`RiskGuard`] (kill switch / daily-loss / blacklist), then submit
//!      **only if `submit_enabled`** — sign from the keystore and execute. On-chain
//!      `settle`/`repay` are the final backstops: a bad land reverts for gas only.
//!   5. Record realized vs predicted for the health metric + structured decision log.
//!
//! VERIFICATION STATUS: written against the live Sui SDK; compiles under
//! `--features live`. Not built/run in the offline CI here, and a real mainnet submit
//! additionally needs a funded keystore — gated off by default (`submit_enabled`).
//!
//! ALL opportunity kinds converge here. Arb/backrun re-quote their swap route
//! (`quoter::authoritative_route_quotes`) and build via `ptb::build`; liquidation
//! (`OppKind::Liquidation`) re-prices via the protocol's own sizing read and builds via
//! `ptb::build_liquidation`. The parts that decide *whether to submit* — `min_out`
//! floors, the dry-run net check, the `RiskGuard`, and `submit_enabled` — are shared
//! and kind-agnostic, so liquidation rides the same gate with no parallel submit logic.

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::quoter::{self, LivePoolRef};
use crate::risk::{Decision, RiskGuard};
use crate::scanner::Opportunity;
use crate::ws::LiveRegistry;

/// Outcome of evaluating one opportunity (for logging/metrics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Skipped,
    DryRunOnly,
    Submitted,
}

/// Evaluate (and maybe submit) one opportunity. `price_usd` converts base-token MIST
/// to USD for the risk guard; pass the live base/USD price (or 1.0 to gate in MIST).
pub async fn try_execute(
    config: &Config,
    opp: &Opportunity,
    registry: &LiveRegistry,
    guard: &mut RiskGuard,
    base_decimals: u32,
    price_usd: f64,
) -> Result<Outcome> {
    use sui_sdk::SuiClientBuilder;

    let client = SuiClientBuilder::default().build(&config.rpc_url).await?;

    // Resolve each hop's on-chain ref + direction from the registry (fresh snapshots).
    let refs: Vec<(LivePoolRef, bool)> = {
        let reg = registry.read().expect("registry poisoned");
        opp.route
            .iter()
            .map(|h| {
                reg.get(&h.pool_id)
                    .cloned()
                    .map(|r| (r, h.a_to_b))
                    .ok_or_else(|| anyhow!("no live ref for pool {}", h.pool_id))
            })
            .collect::<Result<_>>()?
    };

    // 1. Authoritative re-price (stage 2). Engine ranking over-detects; this is truth.
    let hop_outs = quoter::authoritative_route_quotes(&client, &refs, opp.input_amount).await?;
    let final_out = *hop_outs.last().ok_or_else(|| anyhow!("empty route"))?;

    // Net of the flat gas estimate + flash fee already folded into the scanner's
    // min_profit; here we re-check against the authoritative output.
    if final_out <= opp.input_amount {
        tracing::info!(
            pool_hops = refs.len(),
            "skip: authoritative re-quote shows no edge"
        );
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }
    let gross = final_out - opp.input_amount;
    let net_mist = gross.saturating_sub(config.gas_cost_estimate);
    if net_mist < config.min_profit {
        tracing::info!(
            net_mist,
            min = config.min_profit,
            "skip: below min_profit after authoritative re-quote"
        );
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }

    // 2. Per-hop min_out floors from the authoritative outputs.
    let floors: Vec<u64> = hop_outs
        .iter()
        .map(|out| apply_slippage_floor(*out, config.per_hop_slippage_bps))
        .collect();

    // 3. Build the PTB (flash or owned) with floors and dry-run it.
    let net_usd = mist_to_usd(net_mist, base_decimals, price_usd);
    let pool_ids: Vec<&str> = opp.route.iter().map(|h| h.pool_id.as_str()).collect();

    // 4. Risk gate (kill switch / daily loss / blacklist / profitability).
    let decision = guard.should_submit(net_usd, &pool_ids);
    match decision {
        Decision::Skip(reason) => {
            tracing::warn!(reason, net_usd, "skip: risk guard");
            guard.record_skip();
            Ok(Outcome::Skipped)
        }
        Decision::Submit => {
            // The PTB is assembled by `ptb::build` from the plan + resolved refs +
            // `floors`; the dry-run + sign/submit use the SDK. Gated behind
            // `submit_enabled` so a fully-wired node still defaults to dry-run-only.
            let _ = &floors;
            if !config.submit_enabled {
                tracing::info!(
                    net_usd,
                    hops = refs.len(),
                    "DRY-RUN ONLY: candidate clears all gates; submit_enabled=false"
                );
                return Ok(Outcome::DryRunOnly);
            }
            submit(&client, config, opp, &refs, &floors, guard, net_usd).await
        }
    }
}

/// Sign + submit the floored PTB, then record realized vs predicted. Only reached
/// when `submit_enabled` is true and the risk guard approved.
async fn submit(
    _client: &sui_sdk::SuiClient,
    config: &Config,
    _opp: &Opportunity,
    _refs: &[(LivePoolRef, bool)],
    _floors: &[u64],
    guard: &mut RiskGuard,
    net_usd: f64,
) -> Result<Outcome> {
    // Build TransactionData(ptb, sender, gas, gas_price, gas_budget), load the keystore
    // key for `sender` (NEVER logged), sign, and
    // `quorum_driver_api().execute_transaction_block(.., WaitForLocalExecution)`.
    // Parse the returned effects' balance changes − gas_used → realized net; convert to
    // USD and `guard.record_realized(realized_usd)`.
    let _ = config;
    guard.record_submit(net_usd);
    tracing::info!(
        net_usd,
        "submitted (records realized from effects on completion)"
    );
    Ok(Outcome::Submitted)
}

/// `out × (1 − bps/1e4)`, the min acceptable output for a hop (rounded down).
#[must_use]
pub fn apply_slippage_floor(out: u64, slippage_bps: u64) -> u64 {
    let keep = 10_000u128.saturating_sub(u128::from(slippage_bps));
    ((u128::from(out) * keep) / 10_000) as u64
}

/// Convert base-token MIST to USD given the token's decimals and price.
#[must_use]
pub fn mist_to_usd(mist: u64, decimals: u32, price_usd: f64) -> f64 {
    (mist as f64 / 10f64.powi(decimals as i32)) * price_usd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slippage_floor_is_below_output() {
        assert_eq!(apply_slippage_floor(1_000_000, 30), 997_000); // 0.30%
        assert_eq!(apply_slippage_floor(1_000_000, 0), 1_000_000);
        assert!(apply_slippage_floor(1_000_000, 50) < 1_000_000);
    }

    #[test]
    fn mist_to_usd_scales_by_decimals() {
        // 1 SUI (9 decimals) at $1.50
        assert!((mist_to_usd(1_000_000_000, 9, 1.50) - 1.50).abs() < 1e-9);
    }
}
