//! Dry-run + submission (feature = "live").
//!
//! Real submit-only-if-profitable path:
//!   1. Resolve fresh refs; authoritatively re-quote the route (`quoter`). If the edge
//!      closed, skip — no gas.
//!   2. Derive per-hop `min_out` floors from the authoritative outputs.
//!   3. **Build the real PTB** (`ptb::build` / `build_liquidation`) with those floors,
//!      wrap in `TransactionData`, and **`dry_run_transaction_block`** it. Require
//!      `effects.status == Success` AND dry-run net (base-coin balance change, which
//!      nets gas for a SUI base) ≥ `min_profit`. Gas comes from the dry-run, not a flat
//!      constant.
//!   4. Consult the [`RiskGuard`]. Then **only if `submit_enabled`**: load the signing
//!      key from a file keystore (never logged), sign, `execute_transaction_block`
//!      (WaitForLocalExecution), parse landed effects, and `record_realized`.
//!   5. With `submit_enabled = false` (default) the path still builds + dry-runs, then
//!      stops before signing — so the dry-run is always exercised.
//!
//! VERIFICATION STATUS: compiles under `--features live` against sui-sdk
//! `mainnet-v1.73.2`. A real submit additionally needs a published package + a funded
//! keystore + live pools (testnet, Phase 5); not exercised in offline CI here.

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Config;
use crate::quoter::{self, LivePoolRef};
use crate::risk::{Decision, RiskGuard};
use crate::scanner::{OppKind, Opportunity};
use crate::ws::LiveRegistry;

use sui_json_rpc_types::{
    SuiObjectDataOptions, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponseOptions,
};
use sui_sdk::SuiClient;
use sui_types::base_types::{ObjectID, ObjectRef, SequenceNumber, SuiAddress};
use sui_types::object::Owner;
use sui_types::transaction::{
    ObjectArg, ProgrammableTransaction, SharedObjectMutability, Transaction, TransactionData,
};
use sui_types::transaction_driver_types::ExecuteTransactionRequestType;
use sui_types::TypeTag;

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

    // 1. Resolve hop refs + authoritative re-quote (stage 2). Engine ranking over-detects.
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
    let hop_outs = quoter::authoritative_route_quotes(&client, &refs, opp.input_amount).await?;
    let final_out = *hop_outs.last().ok_or_else(|| anyhow!("empty route"))?;
    if final_out <= opp.input_amount {
        tracing::info!("skip: authoritative re-quote shows no edge");
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }

    // 2. Per-hop min_out floors from the authoritative outputs (never 0).
    let floors: Vec<u64> = hop_outs
        .iter()
        .map(|out| apply_slippage_floor(*out, config.per_hop_slippage_bps))
        .collect();

    // 3. Build the REAL PTB (this is what was previously orphaned), then dry-run it.
    let sender: SuiAddress = config
        .sender_address
        .parse()
        .context("ARB_SENDER_ADDRESS")?;
    let base_type: TypeTag = config.base_token.parse().context("base token type")?;
    let pt = build_ptb(&client, config, opp, &refs, &floors, sender).await?;

    let gas_price = client.read_api().get_reference_gas_price().await?;
    let gas_coin = pick_gas_coin(&client, sender).await?;
    let tx_data =
        TransactionData::new_programmable(sender, vec![gas_coin], pt, config.gas_budget, gas_price);

    let dry = dry_run(&client, &tx_data, &base_type, sender).await?;
    if !dry.success {
        tracing::warn!(?dry, "skip: dry-run reverted");
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }
    // Real gas from the dry-run; net is the base-coin balance change (nets gas for SUI base).
    if dry.net_base < i128::from(config.min_profit) {
        tracing::info!(
            net = dry.net_base,
            gas = dry.gas_used,
            "skip: below min_profit after dry-run"
        );
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }

    // 4. Risk gate.
    let net_usd = mist_to_usd(dry.net_base.max(0) as u64, base_decimals, price_usd);
    let pool_ids: Vec<&str> = opp.route.iter().map(|h| h.pool_id.as_str()).collect();
    match guard.should_submit(net_usd, &pool_ids) {
        Decision::Skip(reason) => {
            tracing::warn!(reason, net_usd, "skip: risk guard");
            guard.record_skip();
            Ok(Outcome::Skipped)
        }
        Decision::Submit => {
            if !config.submit_enabled {
                tracing::info!(
                    net_base = dry.net_base,
                    gas = dry.gas_used,
                    net_usd,
                    "DRY-RUN ONLY: clears all gates; submit_enabled=false — stopping before signing"
                );
                return Ok(Outcome::DryRunOnly);
            }
            submit(
                &client,
                config,
                tx_data,
                &base_type,
                sender,
                guard,
                net_usd,
                base_decimals,
                price_usd,
            )
            .await
        }
    }
}

/// Build the PTB for this opportunity: liquidation → `build_liquidation`, otherwise the
/// flash-arb `build`. Resolves all shared-object refs fresh from chain.
async fn build_ptb(
    client: &SuiClient,
    config: &Config,
    opp: &Opportunity,
    refs: &[(LivePoolRef, bool)],
    floors: &[u64],
    sender: SuiAddress,
) -> Result<ProgrammableTransaction> {
    let base_type: TypeTag = config.base_token.parse()?;
    let pkg: ObjectID = config.package_id.parse().context("ARB_PACKAGE_ID")?;

    if opp.kind == OppKind::Liquidation {
        return build_liquidation_ptb(client, config, opp, refs, floors, sender).await;
    }

    // Flash-arb path (ptb::live::build): borrow → begin → swaps → settle_and_return → repay.
    let provider = crate::flashloan::provider_from(
        &config.flash_provider,
        &config.scallop_package_id,
        &config.flash_lender_id,
        config.flash_fee_bps,
        &config.flash_version_id,
    )
    .ok_or_else(|| anyhow!("unknown flash provider '{}'", config.flash_provider))?;
    let plan = crate::ptb::flash_arb_plan(provider.as_ref(), opp, config.min_profit);

    let clock = clock_arg();
    let cetus_cfg = resolve_shared(client, &config.cetus_global_config_id, false)
        .await
        .ok();
    let turbos_ver = resolve_shared(client, &config.turbos_versioned_id, false)
        .await
        .ok();

    let mut hops = Vec::with_capacity(refs.len());
    for (i, (lref, a_to_b)) in refs.iter().enumerate() {
        let pool = ObjectArg::SharedObject {
            id: lref.pool_id,
            initial_shared_version: lref.init_shared_version,
            mutability: SharedObjectMutability::Mutable,
        };
        let mut type_args = vec![lref.type_a.clone(), lref.type_b.clone()];
        if let Some(ft) = &lref.fee_type {
            type_args.push(ft.clone());
        }
        hops.push(crate::ptb::ResolvedHop {
            dex: lref.dex,
            a_to_b: *a_to_b,
            pool,
            type_args,
            min_out: floors[i],
            cetus_config: cetus_cfg,
            clock: Some(clock),
            turbos_versioned: turbos_ver,
        });
    }

    let lender = resolve_shared(client, &config.flash_lender_id, true).await?;
    // Provider shape drives the flash call convention: mock vault calls our package's
    // `flash` module; Scallop calls its `flash_loan` module (needs the Version object).
    let style = provider.style();
    let (provider_package, version) = match style {
        crate::flashloan::FlashStyle::MockVault => (pkg, None),
        crate::flashloan::FlashStyle::Scallop => (
            config.scallop_package_id.parse()?,
            Some(resolve_shared(client, &config.flash_version_id, false).await?),
        ),
    };
    let inputs = crate::ptb::BuildInputs {
        package: pkg,
        provider_package,
        base_type,
        lender,
        amount: opp.input_amount,
        min_profit: config.min_profit,
        hops,
        sender,
        flash_style: style,
        version,
        repay_total: provider.repay_total(opp.input_amount),
    };
    crate::ptb::build(&plan, inputs)
}

/// Assemble + build the Scallop liquidation PTB (flash → in-PTB oracle refresh →
/// liquidate → swap-back → settle_and_return → repay). Object ids come from config;
/// every shared object's initial version is resolved fresh on-chain. Still gated by
/// `submit_enabled` upstream; the accumulator→`HotPotatoVector` Pyth update is the
/// remaining on-chain item (see docs/testnet-runbook.md).
async fn build_liquidation_ptb(
    client: &SuiClient,
    config: &Config,
    opp: &Opportunity,
    refs: &[(LivePoolRef, bool)],
    floors: &[u64],
    sender: SuiAddress,
) -> Result<ProgrammableTransaction> {
    let leg = opp
        .liquidation
        .as_ref()
        .ok_or_else(|| anyhow!("liquidation opp missing leg"))?;
    let debt_type: TypeTag = leg.debt_type.parse().context("debt type")?;
    let collateral_type: TypeTag = leg.collateral_type.parse().context("collateral type")?;

    let provider = crate::flashloan::provider_from(
        &config.flash_provider,
        &config.scallop_package_id,
        &config.flash_lender_id,
        config.flash_fee_bps,
        &config.flash_version_id,
    )
    .ok_or_else(|| anyhow!("unknown flash provider '{}'", config.flash_provider))?;
    let plan = crate::ptb::liquidation_plan(Some(provider.as_ref()), opp, config.min_profit);
    let repay_total = provider.repay_total(leg.repay_amount);

    // swap-back hop: seized collateral → debt asset (first route hop).
    let (lref, a_to_b) = refs
        .first()
        .ok_or_else(|| anyhow!("liquidation route has no swap hop"))?;
    let clock = clock_arg();
    let cetus_cfg = resolve_shared(client, &config.cetus_global_config_id, false)
        .await
        .ok();
    let turbos_ver = resolve_shared(client, &config.turbos_versioned_id, false)
        .await
        .ok();
    let mut type_args = vec![lref.type_a.clone(), lref.type_b.clone()];
    if let Some(ft) = &lref.fee_type {
        type_args.push(ft.clone());
    }
    let swap = crate::ptb::ResolvedHop {
        dex: lref.dex,
        a_to_b: *a_to_b,
        pool: ObjectArg::SharedObject {
            id: lref.pool_id,
            initial_shared_version: lref.init_shared_version,
            mutability: SharedObjectMutability::Mutable,
        },
        type_args,
        min_out: *floors.first().unwrap_or(&0),
        cetus_config: cetus_cfg,
        clock: Some(clock),
        turbos_versioned: turbos_ver,
    };

    let debt_pi = config
        .price_info_object(&leg.debt_type)
        .ok_or_else(|| anyhow!("no PriceInfoObject configured for debt {}", leg.debt_type))?;
    let coll_pi = config
        .price_info_object(&leg.collateral_type)
        .ok_or_else(|| {
            anyhow!(
                "no PriceInfoObject configured for collateral {}",
                leg.collateral_type
            )
        })?;

    let inputs = crate::ptb::LiquidationInputs {
        package: config.package_id.parse().context("ARB_PACKAGE_ID")?,
        scallop_package: config.scallop_package_id.parse()?,
        debt_type,
        collateral_type,
        version: resolve_shared(client, &config.flash_version_id, false).await?,
        obligation: resolve_shared(client, &leg.obligation_id, true).await?,
        market: resolve_shared(client, &config.flash_lender_id, true).await?,
        registry: resolve_shared(client, &config.scallop_registry_id, false).await?,
        x_oracle: resolve_shared(client, &config.scallop_x_oracle_id, true).await?,
        clock,
        x_oracle_package: config
            .x_oracle_package_id
            .parse()
            .context("ARB_X_ORACLE_PACKAGE_ID")?,
        pyth_rule_package: config
            .pyth_rule_package_id
            .parse()
            .context("ARB_PYTH_RULE_PACKAGE_ID")?,
        pyth_state: resolve_shared(client, &config.pyth_state_id, false).await?,
        pyth_registry: resolve_shared(client, &config.scallop_pyth_registry_id, false).await?,
        debt_price_info: resolve_shared(client, debt_pi, false).await?,
        collateral_price_info: resolve_shared(client, coll_pi, false).await?,
        repay_amount: leg.repay_amount,
        repay_total,
        min_profit: config.min_profit,
        swap,
        sender,
    };
    crate::ptb::build_liquidation(&plan, inputs)
}

/// Resolve a shared object's `ObjectArg` (fetches its initial shared version fresh).
async fn resolve_shared(client: &SuiClient, id_str: &str, mutable: bool) -> Result<ObjectArg> {
    let id: ObjectID = id_str
        .parse()
        .with_context(|| format!("object id {id_str}"))?;
    let resp = client
        .read_api()
        .get_object_with_options(id, SuiObjectDataOptions::new().with_owner())
        .await?;
    let data = resp
        .data
        .ok_or_else(|| anyhow!("object {id_str} not found"))?;
    let initial_shared_version = match data.owner {
        Some(Owner::Shared {
            initial_shared_version,
        }) => initial_shared_version,
        other => bail!("object {id_str} is not shared: {other:?}"),
    };
    Ok(ObjectArg::SharedObject {
        id,
        initial_shared_version,
        mutability: if mutable {
            SharedObjectMutability::Mutable
        } else {
            SharedObjectMutability::Immutable
        },
    })
}

/// The system `Clock` at 0x6 (shared, immutable, initial version 1).
fn clock_arg() -> ObjectArg {
    ObjectArg::SharedObject {
        id: ObjectID::from_hex_literal("0x6").expect("clock id"),
        initial_shared_version: SequenceNumber::from_u64(1),
        mutability: SharedObjectMutability::Immutable,
    }
}

/// Pick the largest owned SUI coin as the gas payment.
async fn pick_gas_coin(client: &SuiClient, sender: SuiAddress) -> Result<ObjectRef> {
    let coins = client
        .coin_read_api()
        .get_coins(sender, None, None, Some(50))
        .await?;
    let coin = coins
        .data
        .into_iter()
        .max_by_key(|c| c.balance)
        .ok_or_else(|| anyhow!("no SUI coins to pay gas for {sender}"))?;
    Ok(coin.object_ref())
}

#[derive(Debug)]
struct DryRun {
    success: bool,
    net_base: i128,
    gas_used: u64,
}

/// Dry-run the assembled transaction; extract success, the sender's net base-coin
/// balance change, and the gas used (real, not the flat constant).
async fn dry_run(
    client: &SuiClient,
    tx_data: &TransactionData,
    base_type: &TypeTag,
    sender: SuiAddress,
) -> Result<DryRun> {
    let resp = client
        .read_api()
        .dry_run_transaction_block(tx_data.clone())
        .await?;
    let success = resp.effects.status().is_ok();
    let g = resp.effects.gas_cost_summary();
    let gas_used = g.computation_cost + g.storage_cost
        - g.storage_rebate.min(g.storage_cost + g.computation_cost);
    let net_base = net_base_change(&resp.balance_changes, base_type, sender);
    Ok(DryRun {
        success,
        net_base,
        gas_used,
    })
}

/// Sum the sender's balance change in the base coin (i128; negative = spent).
fn net_base_change(
    changes: &[sui_json_rpc_types::BalanceChange],
    base_type: &TypeTag,
    sender: SuiAddress,
) -> i128 {
    changes
        .iter()
        .filter(|c| &c.coin_type == base_type && owner_is(&c.owner, sender))
        .map(|c| c.amount)
        .sum()
}

fn owner_is(owner: &Owner, addr: SuiAddress) -> bool {
    matches!(owner, Owner::AddressOwner(a) if *a == addr)
}

/// Sign with the file keystore and submit; parse landed effects → `record_realized`.
/// Only reached when `submit_enabled` is true and the risk guard approved.
#[allow(clippy::too_many_arguments)]
async fn submit(
    client: &SuiClient,
    config: &Config,
    tx_data: TransactionData,
    base_type: &TypeTag,
    sender: SuiAddress,
    guard: &mut RiskGuard,
    predicted_usd: f64,
    base_decimals: u32,
    price_usd: f64,
) -> Result<Outcome> {
    use shared_crypto::intent::Intent;
    use std::path::PathBuf;
    use sui_keys::keystore::{AccountKeystore, FileBasedKeystore};

    if config.keystore_path.is_empty() {
        bail!("submit_enabled but ARB_KEYSTORE_PATH is unset");
    }
    let keystore = FileBasedKeystore::load_or_create(&PathBuf::from(&config.keystore_path))?;
    // The signing key never leaves the keystore and is never logged.
    let sig = keystore
        .sign_secure(&sender, &tx_data, Intent::sui_transaction())
        .await?;
    let tx = Transaction::from_data(tx_data, vec![sig]);

    let opts = SuiTransactionBlockResponseOptions::new()
        .with_effects()
        .with_balance_changes();
    let resp = client
        .quorum_driver_api()
        .execute_transaction_block(
            tx,
            opts,
            Some(ExecuteTransactionRequestType::WaitForLocalExecution),
        )
        .await?;

    guard.record_submit(predicted_usd);
    // Realized = sender's base-coin balance change from the LANDED effects (negative if
    // the trade reverted / lost the race and only cost gas).
    let realized_base = resp
        .balance_changes
        .as_deref()
        .map(|c| net_base_change(c, base_type, sender))
        .unwrap_or(0);
    let realized_usd = mist_to_usd(realized_base.max(0) as u64, base_decimals, price_usd)
        - if realized_base < 0 {
            mist_to_usd((-realized_base) as u64, base_decimals, price_usd)
        } else {
            0.0
        };
    guard.record_realized(realized_usd);
    tracing::info!(
        digest = ?resp.digest,
        realized_base,
        realized_usd,
        "submitted; realized recorded"
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
        assert!((mist_to_usd(1_000_000_000, 9, 1.50) - 1.50).abs() < 1e-9);
    }
}
