//! Dry-run + submission (feature = "live").
//!
//! Real submit-only-if-profitable path:
//!   1. Resolve hop refs from the hot registry (no network) and derive per-hop `min_out`
//!      floors from the stage-1 simulated hop outputs.
//!   2. **Build the real PTB** (`ptb::build` / `build_liquidation`) with those floors,
//!      wrap in `TransactionData`, and **`dry_run_transaction_block`** it — the single
//!      authoritative re-quote. Require `effects.status == Success` AND dry-run net
//!      (base-coin balance change, which nets gas for a SUI base) ≥ `min_profit`. Gas comes
//!      from the dry-run, not a flat constant.
//!   3. Consult the [`RiskGuard`]. Then **only if `submit_enabled`**: load the signing
//!      key from a file keystore (never logged), sign, `execute_transaction_block`
//!      (WaitForLocalExecution), parse landed effects, and `record_realized`.
//!   4. With `submit_enabled = false` (default) the path still builds + dry-runs, then
//!      stops before signing — so the dry-run is always exercised.
//!
//! [`validate_opportunity`] exposes step 1–2 (build + dry-run, **never** submit) as a
//! reusable pre-submit gate — used by the `liq-validate` harness.
//!
//! VERIFICATION STATUS: compiles under `--features live` against sui-sdk
//! `mainnet-v1.73.2`. A real submit additionally needs a published package + a funded
//! keystore + live pools; not exercised in offline CI here.

use anyhow::{anyhow, bail, Context, Result};
use std::time::Instant;

use crate::config::Config;
use crate::metrics;
use crate::objcache::ObjRefCache;
use crate::quoter::LivePoolRef;
use crate::risk::{Decision, RiskGuard};
use crate::scanner::{OppKind, Opportunity};
use crate::ws::LiveRegistry;

use sui_json_rpc_types::{SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponseOptions};
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
#[allow(clippy::too_many_arguments)]
pub async fn try_execute(
    client: &SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    opp: &Opportunity,
    registry: &LiveRegistry,
    guard: &mut RiskGuard,
    base_decimals: u32,
    price_usd: f64,
) -> Result<Outcome> {
    let _span = tracing::info_span!("execute", kind = ?opp.kind, hops = opp.route.len()).entered();
    // 1. Resolve hop refs from the hot registry — no network (pool versions are kept
    //    fresh by the ingestion task).
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

    // 2. Per-hop min_out floors from the stage-1 simulated hop outputs. The authoritative
    //    re-quote is now the single on-chain `dry_run` of the REAL PTB below — we no
    //    longer issue a separate sequential per-hop `devInspect` round-trip (that was
    //    ~300–500ms and redundant with the dry-run, which prices the exact PTB and gates
    //    on `net_base >= min_profit`).
    if opp.hop_outputs.len() != opp.route.len() {
        bail!(
            "opportunity hop_outputs ({}) != route hops ({})",
            opp.hop_outputs.len(),
            opp.route.len()
        );
    }
    let floors: Vec<u64> = opp
        .hop_outputs
        .iter()
        .map(|out| apply_slippage_floor(*out, config.per_hop_slippage_bps))
        .collect();

    // 3. Build the REAL PTB (this is what was previously orphaned), then dry-run it.
    let sender: SuiAddress = config
        .sender_address
        .parse()
        .context("ARB_SENDER_ADDRESS")?;
    let base_type: TypeTag = config.base_token.parse().context("base token type")?;
    let pt = {
        let _t = metrics::stage_timer(metrics::Stage::PtbBuild);
        build_ptb(client, objcache, config, opp, &refs, &floors, sender).await?
    };

    // These two reads are independent — run them concurrently.
    let (gas_price, gas_coin) = tokio::join!(
        metrics::time_rpc(
            metrics::Rpc::GasPrice,
            client.read_api().get_reference_gas_price()
        ),
        pick_gas_coin(client, sender),
    );
    let (gas_price, gas_coin) = (gas_price?, gas_coin?);
    let tx_data =
        TransactionData::new_programmable(sender, vec![gas_coin], pt, config.gas_budget, gas_price);

    let dry = {
        let _t = metrics::stage_timer(metrics::Stage::DryRun);
        dry_run(client, &tx_data, &base_type, sender).await?
    };
    let dry_run_at = Instant::now();
    metrics::inc_dry_run(dry.success);
    if !dry.success {
        tracing::warn!(?dry, "skip: dry-run reverted");
        metrics::inc_rejected(metrics::Reject::DryRunRevert);
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }
    // Real gas from the dry-run; net is the base-coin balance change (nets gas for SUI base).
    let effective_min = match opp.kind {
        OppKind::Arb | OppKind::Backrun => config.effective_min_profit_mist(),
        OppKind::Liquidation => config.min_profit,
    };
    if dry.net_base < i128::from(effective_min) {
        tracing::info!(
            net = dry.net_base,
            gas = dry.gas_used,
            "skip: below min_profit after dry-run"
        );
        metrics::inc_rejected(metrics::Reject::BelowMinProfit);
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }

    // 4. Risk gate.
    let net_usd = mist_to_usd(dry.net_base.max(0) as u64, base_decimals, price_usd);
    let pool_ids: Vec<&str> = opp.route.iter().map(|h| h.pool_id.as_str()).collect();
    match guard.should_submit(net_usd, &pool_ids) {
        Decision::Skip(reason) => {
            tracing::warn!(reason, net_usd, "skip: risk guard");
            metrics::inc_rejected(match reason {
                "blacklisted_pool" => metrics::Reject::Blacklisted,
                "not_profitable" => metrics::Reject::NotProfitable,
                _ => metrics::Reject::RiskGuard,
            });
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
                client,
                config,
                tx_data,
                &base_type,
                sender,
                guard,
                dry.net_base,
                net_usd,
                base_decimals,
                price_usd,
                dry_run_at,
            )
            .await
        }
    }
}

/// Result of validating one opportunity with [`validate_opportunity`] — build + dry-run,
/// never signed/submitted.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// PTB was assembled and a dry-run was obtained (vs. failing before that).
    pub built: bool,
    /// The dry-run's on-chain effects status was success.
    pub dry_run_success: bool,
    /// Stage-1 predicted net (base MIST) from the scanner/sizer.
    pub predicted_net: i128,
    /// Dry-run simulated net (sender's base-coin balance change, nets gas for a SUI base).
    pub simulated_net: i128,
    /// Real gas used from the dry-run.
    pub gas_used: u64,
    /// Set when build or dry-run failed; the reason.
    pub failure: Option<String>,
}

/// Build the **production** PTB for `opp` and `dry_run` it — **never signs or submits**.
/// Reusable as the final pre-submit validation gate (see the `liq-validate` harness and
/// docs/scallop-liquidation-verified.md §8). Captures every failure as a string rather than
/// erroring, so a caller can log per-opportunity outcomes without aborting the loop.
pub async fn validate_opportunity(
    client: &SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    opp: &Opportunity,
    registry: &LiveRegistry,
) -> ValidationReport {
    let mut report = ValidationReport {
        built: false,
        dry_run_success: false,
        predicted_net: i128::from(opp.net_profit),
        simulated_net: 0,
        gas_used: 0,
        failure: None,
    };
    match build_and_dry_run(client, objcache, config, opp, registry).await {
        Ok(dry) => {
            report.built = true;
            report.dry_run_success = dry.success;
            report.simulated_net = dry.net_base;
            report.gas_used = dry.gas_used;
            if !dry.success {
                report.failure = Some("dry-run effects status: failure".to_string());
            }
        }
        Err(e) => report.failure = Some(format!("{e:#}")),
    }
    report
}

/// Shared core: resolve refs → floors → build the real PTB → dry-run. Used by both
/// `try_execute` (then gated/submitted) and `validate_opportunity` (never submitted).
async fn build_and_dry_run(
    client: &SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    opp: &Opportunity,
    registry: &LiveRegistry,
) -> Result<DryRun> {
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
    if opp.hop_outputs.len() != opp.route.len() {
        bail!(
            "opportunity hop_outputs ({}) != route hops ({})",
            opp.hop_outputs.len(),
            opp.route.len()
        );
    }
    let floors: Vec<u64> = opp
        .hop_outputs
        .iter()
        .map(|out| apply_slippage_floor(*out, config.per_hop_slippage_bps))
        .collect();
    let sender: SuiAddress = config
        .sender_address
        .parse()
        .context("ARB_SENDER_ADDRESS")?;
    let base_type: TypeTag = config.base_token.parse().context("base token type")?;
    let pt = {
        let _t = metrics::stage_timer(metrics::Stage::PtbBuild);
        build_ptb(client, objcache, config, opp, &refs, &floors, sender).await?
    };
    let (gas_price, gas_coin) = tokio::join!(
        metrics::time_rpc(
            metrics::Rpc::GasPrice,
            client.read_api().get_reference_gas_price()
        ),
        pick_gas_coin(client, sender),
    );
    let (gas_price, gas_coin) = (gas_price?, gas_coin?);
    let tx_data =
        TransactionData::new_programmable(sender, vec![gas_coin], pt, config.gas_budget, gas_price);
    let dry = {
        let _t = metrics::stage_timer(metrics::Stage::DryRun);
        dry_run(client, &tx_data, &base_type, sender).await?
    };
    metrics::inc_dry_run(dry.success);
    Ok(dry)
}

/// Build the PTB for this opportunity: liquidation → `build_liquidation`, otherwise the
/// flash-arb `build`. Resolves all shared-object refs fresh from chain.
#[allow(clippy::too_many_arguments)]
async fn build_ptb(
    client: &SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    opp: &Opportunity,
    refs: &[(LivePoolRef, bool)],
    floors: &[u64],
    sender: SuiAddress,
) -> Result<ProgrammableTransaction> {
    let base_type: TypeTag = config.base_token.parse()?;
    let pkg: ObjectID = config.package_id.parse().context("ARB_PACKAGE_ID")?;

    if opp.kind == OppKind::Liquidation {
        return build_liquidation_ptb(client, objcache, config, opp, refs, floors, sender).await;
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
    let effective_min = config.effective_min_profit_mist();
    let plan = crate::ptb::flash_arb_plan(provider.as_ref(), opp, effective_min);

    let clock = clock_arg();
    let cetus_cfg = objcache
        .shared_arg(client, &config.cetus_global_config_id, false)
        .await
        .ok();
    let turbos_ver = objcache
        .shared_arg(client, &config.turbos_versioned_id, false)
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

    let lender = objcache
        .shared_arg(client, &config.flash_lender_id, true)
        .await?;
    // Provider shape drives the flash call convention: mock vault calls our package's
    // `flash` module; Scallop calls its `flash_loan` module (needs the Version object).
    let style = provider.style();
    let (provider_package, version) = match style {
        crate::flashloan::FlashStyle::MockVault => (pkg, None),
        crate::flashloan::FlashStyle::Scallop => (
            config.scallop_package_id.parse()?,
            Some(
                objcache
                    .shared_arg(client, &config.flash_version_id, false)
                    .await?,
            ),
        ),
    };
    let inputs = crate::ptb::BuildInputs {
        package: pkg,
        provider_package,
        base_type,
        lender,
        amount: opp.input_amount,
        min_profit: effective_min,
        hops,
        sender,
        flash_style: style,
        version,
        repay_total: provider.repay_total(opp.input_amount),
    };
    crate::ptb::build(&plan, inputs)
}

/// Build the OWNED-capital PTB for `opp` at `opp.input_amount`: split input off the gas coin
/// → `executor::begin` → swaps → `executor::settle` (profit gate + return to sender). No flash
/// borrow/repay, no Scallop. `floors` are the per-hop `min_out` slippage floors. `min_profit`
/// is the on-chain settle threshold.
#[allow(clippy::too_many_arguments)]
async fn build_owned_ptb(
    client: &SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    route: &[crate::scanner::Hop],
    refs: &[(LivePoolRef, bool)],
    floors: &[u64],
    amount: u64,
    min_profit: u64,
) -> Result<ProgrammableTransaction> {
    let base_type: TypeTag = config.base_token.parse()?;
    let pkg: ObjectID = config.package_id.parse().context("ARB_PACKAGE_ID")?;
    let clock = clock_arg();
    let cetus_cfg = objcache
        .shared_arg(client, &config.cetus_global_config_id, false)
        .await
        .ok();
    let turbos_ver = objcache
        .shared_arg(client, &config.turbos_versioned_id, false)
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

    // The plan describes Begin → swaps → Settle. push_swaps reads each hop's dex + a_to_b to
    // pick the adapter module/function, so the plan MUST use the real route.
    let plan_opp = crate::scanner::Opportunity {
        kind: OppKind::Arb,
        liquidation: None,
        route: route.to_vec(),
        input_amount: amount,
        output_amount: 0,
        hop_outputs: vec![0; route.len()],
        gross_profit: 0,
        flash_fee: 0,
        net_profit: 0,
    };
    let plan = crate::ptb::owned_arb_plan(&plan_opp, min_profit);
    let inputs = crate::ptb::OwnedInputs {
        package: pkg,
        base_type,
        amount,
        min_profit,
        hops,
    };
    crate::ptb::build_owned(&plan, inputs)
}

/// Gas payment coins whose balances sum to at least `need` (largest-first), so an owned-mode
/// PTB can split the input off the (smashed) gas coin and still cover gas. Errors if the
/// wallet can't cover `need`.
async fn pick_gas_coins_covering(
    client: &SuiClient,
    sender: SuiAddress,
    need: u64,
) -> Result<Vec<ObjectRef>> {
    let mut coins = client
        .coin_read_api()
        .get_coins(sender, None, None, Some(200))
        .await?
        .data;
    coins.sort_by_key(|c| std::cmp::Reverse(c.balance));
    let mut picked = Vec::new();
    let mut sum: u64 = 0;
    for c in coins {
        if sum >= need {
            break;
        }
        sum += c.balance;
        picked.push(c.object_ref());
    }
    if sum < need || picked.is_empty() {
        bail!("wallet SUI {sum} < required {need} (input + gas) for {sender}");
    }
    Ok(picked)
}

/// One size's outcome in the owned-mode sweep (for the report).
#[derive(Debug, Clone)]
pub struct OwnedSizeResult {
    pub size: u64,
    pub expected_out: u64,
    pub expected_net: i128,
    pub dry_success: bool,
    pub dry_net_base: i128,
    pub dry_gas: u64,
    pub note: String,
}

/// Owned-capital execution with dynamic position sizing. For a fixed `route`, re-quote each
/// configured size (≤ wallet balance and `max_wallet_capital`), then dry-run the
/// positive-expected sizes largest-first and pick the largest that succeeds with
/// `net_base ≥ effective_min_profit`. Submits that one only if `submit_enabled`. Keeps the
/// dry-run gate, slippage floors, risk guard, daily-loss cap, and kill-switch. Returns the
/// per-size results (caller logs/aggregates the report).
#[allow(clippy::too_many_arguments)]
pub async fn try_execute_owned_sized(
    client: &SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    route: &[crate::scanner::Hop],
    pools: &[crate::types::PoolState],
    registry: &LiveRegistry,
    guard: &mut RiskGuard,
    base_decimals: u32,
    price_usd: f64,
) -> Result<Vec<OwnedSizeResult>> {
    let sender: SuiAddress = config
        .sender_address
        .parse()
        .context("ARB_SENDER_ADDRESS")?;
    let base_type: TypeTag = config.base_token.parse().context("base token type")?;
    let effective_min = config.effective_min_profit_mist();

    // Resolve hop refs once (route is fixed across sizes).
    let refs: Vec<(LivePoolRef, bool)> = {
        let reg = registry.read().expect("registry poisoned");
        route
            .iter()
            .map(|h| {
                reg.get(&h.pool_id)
                    .cloned()
                    .map(|r| (r, h.a_to_b))
                    .ok_or_else(|| anyhow!("no live ref for pool {}", h.pool_id))
            })
            .collect::<Result<_>>()?
    };

    // Wallet balance → size cap. Leave the gas budget aside (input is split off the gas coin).
    let balance = client
        .coin_read_api()
        .get_balance(sender, None)
        .await?
        .total_balance;
    let cap = (balance.saturating_sub(u128::from(config.gas_budget)) as u64)
        .min(config.max_wallet_capital);
    tracing::info!(
        wallet_mist = balance as u64,
        cap_mist = cap,
        effective_min_profit = effective_min,
        sizes = ?config.owned_sizes,
        "owned-mode sweep: wallet balance detected"
    );

    // Stage 1: cheap re-quote per size; keep sizes within cap with positive expected net.
    let mut sized: Vec<OwnedSizeResult> = Vec::new();
    for &size in &config.owned_sizes {
        if size > cap {
            sized.push(OwnedSizeResult {
                size,
                expected_out: 0,
                expected_net: 0,
                dry_success: false,
                dry_net_base: 0,
                dry_gas: 0,
                note: "skipped: exceeds wallet cap".into(),
            });
            continue;
        }
        match crate::scanner::simulate_route_hops(pools, route, size) {
            Some(hops) => {
                let out = *hops.last().unwrap_or(&0);
                sized.push(OwnedSizeResult {
                    size,
                    expected_out: out,
                    expected_net: i128::from(out) - i128::from(size),
                    dry_success: false,
                    dry_net_base: 0,
                    dry_gas: 0,
                    note: String::new(),
                });
            }
            None => sized.push(OwnedSizeResult {
                size,
                expected_out: 0,
                expected_net: 0,
                dry_success: false,
                dry_net_base: 0,
                dry_gas: 0,
                note: "quote failed".into(),
            }),
        }
    }

    // Stage 2: dry-run positive-expected sizes BEST-EXPECTED-NET first; the first
    // authoritative success wins. Largest-first can knowingly choose a worse trade
    // after price impact, even when a smaller configured size has higher expected net.
    let gas_price = client.read_api().get_reference_gas_price().await?;
    let mut winner: Option<(u64, TransactionData, i128, Instant)> = None;
    let mut order: Vec<usize> = (0..sized.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sized[i].expected_net));
    for i in order {
        if winner.is_some() || sized[i].expected_net <= 0 || sized[i].expected_out == 0 {
            continue;
        }
        let size = sized[i].size;
        let hop_outs = match crate::scanner::simulate_route_hops(pools, route, size) {
            Some(h) => h,
            None => continue,
        };
        let floors: Vec<u64> = hop_outs
            .iter()
            .map(|o| apply_slippage_floor(*o, config.per_hop_slippage_bps))
            .collect();
        let pt = match build_owned_ptb(
            client,
            objcache,
            config,
            route,
            &refs,
            &floors,
            size,
            effective_min,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                sized[i].note = format!("ptb build failed: {e}");
                continue;
            }
        };
        let gas_coins =
            match pick_gas_coins_covering(client, sender, size + config.gas_budget).await {
                Ok(c) => c,
                Err(e) => {
                    sized[i].note = format!("gas coins: {e}");
                    continue;
                }
            };
        let tx =
            TransactionData::new_programmable(sender, gas_coins, pt, config.gas_budget, gas_price);
        let dry = dry_run(client, &tx, &base_type, sender).await?;
        let dry_run_at = Instant::now();
        sized[i].dry_success = dry.success;
        sized[i].dry_net_base = dry.net_base;
        sized[i].dry_gas = dry.gas_used;
        if !dry.success {
            sized[i].note = "dry-run reverted".into();
        } else if dry.net_base < i128::from(effective_min) {
            sized[i].note = format!("below min_profit (net {} < {effective_min})", dry.net_base);
        } else {
            sized[i].note = "dry-run OK, profitable".into();
            winner = Some((size, tx, dry.net_base, dry_run_at));
        }
        tracing::info!(
            size,
            expected_out = sized[i].expected_out,
            dry_success = sized[i].dry_success,
            dry_net_base = sized[i].dry_net_base,
            dry_gas = sized[i].dry_gas,
            note = %sized[i].note,
            "owned-mode size attempt"
        );
    }

    // Stage 3: submit the winner (only if enabled + risk allows).
    if let Some((size, tx, net_base, dry_run_at)) = winner {
        let net_usd = mist_to_usd(net_base.max(0) as u64, base_decimals, price_usd);
        let pool_ids: Vec<&str> = route.iter().map(|h| h.pool_id.as_str()).collect();
        match guard.should_submit(net_usd, &pool_ids) {
            Decision::Skip(reason) => {
                tracing::warn!(reason, size, "owned-mode: risk guard skip");
            }
            Decision::Submit => {
                if config.submit_enabled {
                    tracing::warn!(size, net_base, "owned-mode: SUBMITTING winning size");
                    submit(
                        client,
                        config,
                        tx,
                        &base_type,
                        sender,
                        guard,
                        net_base,
                        net_usd,
                        base_decimals,
                        price_usd,
                        dry_run_at,
                    )
                    .await?;
                } else {
                    tracing::info!(
                        size,
                        net_base,
                        "owned-mode: DRY-RUN ONLY winner (submit_enabled=false)"
                    );
                }
            }
        }
    } else {
        tracing::info!("owned-mode: no size produced a profitable dry-run");
    }
    Ok(sized)
}

/// Assemble + build the Scallop liquidation PTB (VERIFIED Pyth accumulator refresh →
/// flash borrow → liquidate → swap-back → settle_and_return → repay). Object ids come from
/// config (Wormhole/Pyth defaults are the verified mainnet ids); the Hermes accumulator is
/// fetched + its VAA extracted here. PTB shape is proven by `ptb::live_tests` against the
/// real on-chain liquidation; final gate is a live `dry_run` (docs/scallop-liquidation-verified.md §8).
/// Still gated by `submit_enabled` upstream.
#[allow(clippy::too_many_arguments)]
async fn build_liquidation_ptb(
    client: &SuiClient,
    objcache: &ObjRefCache,
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
    let cetus_cfg = objcache
        .shared_arg(client, &config.cetus_global_config_id, false)
        .await
        .ok();
    let turbos_ver = objcache
        .shared_arg(client, &config.turbos_versioned_id, false)
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

    // VERIFIED Pyth accumulator flow (docs/scallop-liquidation-verified.md §4): fetch ONE
    // Hermes accumulator covering the debt + collateral feeds, extract the embedded VAA, and
    // refresh both PriceInfoObjects in-band before liquidate reads x_oracle.
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
    let debt_feed = config
        .pyth_feed_id(&leg.debt_type)
        .ok_or_else(|| anyhow!("no Pyth feed id for debt {}", leg.debt_type))?;
    let coll_feed = config
        .pyth_feed_id(&leg.collateral_type)
        .ok_or_else(|| anyhow!("no Pyth feed id for collateral {}", leg.collateral_type))?;
    let accumulator_msg = metrics::time_rpc(
        metrics::Rpc::Hermes,
        crate::liquidation::oracle::fetch_pyth_accumulator(
            &config.hermes_url,
            &[debt_feed.to_string(), coll_feed.to_string()],
        ),
    )
    .await?;
    let vaa_bytes = crate::liquidation::oracle::extract_vaa_from_accumulator(&accumulator_msg)?;
    // PriceInfoObjects are mutated by update_single_price_feed → resolve them mutable.
    let price_infos = vec![
        objcache.shared_arg(client, debt_pi, true).await?,
        objcache.shared_arg(client, coll_pi, true).await?,
    ];

    let inputs = crate::ptb::LiquidationInputs {
        package: config.package_id.parse().context("ARB_PACKAGE_ID")?,
        scallop_package: config.scallop_package_id.parse()?,
        debt_type,
        collateral_type,
        version: objcache
            .shared_arg(client, &config.flash_version_id, false)
            .await?,
        obligation: objcache
            .shared_arg(client, &leg.obligation_id, true)
            .await?,
        market: objcache
            .shared_arg(client, &config.flash_lender_id, true)
            .await?,
        registry: objcache
            .shared_arg(client, &config.scallop_registry_id, false)
            .await?,
        // VERIFIED: liquidate takes `&XOracle` (immutable).
        x_oracle: objcache
            .shared_arg(client, &config.scallop_x_oracle_id, false)
            .await?,
        clock,
        wormhole_package: config
            .wormhole_package_id
            .parse()
            .context("ARB_WORMHOLE_PACKAGE_ID")?,
        pyth_package: config
            .pyth_package_id
            .parse()
            .context("ARB_PYTH_PACKAGE_ID")?,
        wormhole_state: objcache
            .shared_arg(client, &config.wormhole_state_id, false)
            .await?,
        pyth_state: objcache
            .shared_arg(client, &config.pyth_state_id, false)
            .await?,
        vaa_bytes,
        accumulator_msg,
        pyth_fee: config.pyth_fee,
        price_infos,
        repay_amount: leg.repay_amount,
        repay_total,
        min_profit: config.min_profit,
        swap,
        sender,
    };
    crate::ptb::build_liquidation(&plan, inputs)
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
    let resp = metrics::time_rpc(
        metrics::Rpc::DryRun,
        client.read_api().dry_run_transaction_block(tx_data.clone()),
    )
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
    predicted_base: i128,
    predicted_usd: f64,
    base_decimals: u32,
    price_usd: f64,
    dry_run_at: Instant,
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

    // The dry-run is our authoritative quote. Do not submit if signing/keystore
    // latency made that quote older than the configured race window.
    if dry_run_at.elapsed().as_millis() > u128::from(config.max_quote_age_ms) {
        tracing::warn!(
            age_ms = dry_run_at.elapsed().as_millis(),
            max_age_ms = config.max_quote_age_ms,
            "skip: authoritative dry-run quote expired before submit"
        );
        metrics::inc_rejected(metrics::Reject::RiskGuard);
        guard.record_skip();
        return Ok(Outcome::Skipped);
    }
    // Capture-ratio accounting must include only transactions that are actually
    // about to be submitted. Dry-run-only and expired quotes have no realized leg.
    metrics::add_predicted_net(predicted_base);
    let tx = Transaction::from_data(tx_data, vec![sig]);

    let opts = SuiTransactionBlockResponseOptions::new()
        .with_effects()
        .with_balance_changes();
    let _submit_timer = metrics::stage_timer(metrics::Stage::Submit);
    let resp = metrics::time_rpc(
        metrics::Rpc::ExecuteTx,
        client.quorum_driver_api().execute_transaction_block(
            tx,
            opts,
            Some(ExecuteTransactionRequestType::WaitForLocalExecution),
        ),
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
    let landed_ok = resp
        .effects
        .as_ref()
        .map(|e| e.status().is_ok())
        .unwrap_or(false);
    metrics::inc_tx(landed_ok);
    metrics::add_realized_net(realized_base);
    tracing::info!(
        digest = ?resp.digest,
        landed_ok,
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
