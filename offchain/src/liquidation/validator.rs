//! Operator validation harness (feature = "liq-validate").
//!
//! The **final pre-submit gate** (docs/scallop-liquidation-verified.md §8): continuously
//! watch the obligation index for underwater positions and, for each, build the *production*
//! liquidation PTB and `dry_run` it on live state — proving the whole pipeline (decode →
//! health → Hermes oracle → PTB assembly → simulation) works end-to-end **before** any real
//! submission. It **never** signs or submits: it only calls [`executor::validate_opportunity`]
//! (build + dry-run), independent of `ARB_SUBMIT_ENABLED`.
//!
//! It reuses every production component unchanged — the same ingestion, obligation index,
//! oracle params, `LiquidationSource`, and PTB builder the live bot uses — so a green run
//! here is direct evidence the live path will build the same transaction.
//!
//! Requirement: the collateral→debt swap-back pool for each candidate must be in
//! `ARB_TRACKED_POOLS` (its `LivePoolRef` is what the builder needs); otherwise the source
//! finds no swap route and the obligation is skipped.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use crate::cache::ReserveCache;
use crate::config::Config;
use crate::liquidation::detect::LiqParams;
use crate::liquidation::source::{self, LiquidationSource, SharedParams};
use crate::liquidation::{self, health, ObligationIndex};
use crate::objcache::ObjRefCache;
use crate::scanner::Opportunity;
use crate::strategy::OpportunitySource;
use crate::ws::{self, LiveRegistry};
use crate::{executor, ingest};

/// Run the dry-run validation loop forever. Never submits.
pub async fn run(config: &Config) -> Result<()> {
    use sui_sdk::SuiClientBuilder;

    let cache = Arc::new(ReserveCache::new());
    let registry: LiveRegistry = Arc::new(RwLock::new(HashMap::new()));
    let client = Arc::new(SuiClientBuilder::default().build(&config.rpc_url).await?);
    let objcache = ObjRefCache::new();
    let index: ObligationIndex = Arc::new(RwLock::new(HashMap::new()));
    let params: SharedParams = Arc::new(RwLock::new(HashMap::new()));
    let metas = source::parse_asset_meta(&config.liq_assets);

    info!(
        tracked_pools = config.tracked_pools.len(),
        liq_assets = metas.len(),
        "liq-validate starting — DRY-RUN ONLY; never signs or submits (ARB_SUBMIT_ENABLED is ignored)"
    );

    // Pools — the swap-back refs the PTB builder needs live in the registry.
    ws::bootstrap_pools(config, &cache, &registry).await?;
    {
        let cache = cache.clone();
        let registry = registry.clone();
        let config = config.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let res = if config.ingest_mode == "checkpoint" {
                ingest::run_checkpoint_diff(&config, &cache, &registry, &client).await
            } else {
                ws::run(&config, &cache, &registry).await
            };
            if let Err(e) = res {
                warn!(error = %e, "pool ingest task exited");
            }
        });
    }
    // Obligation index: bootstrap + reconcile.
    {
        let index = index.clone();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = liquidation::index::bootstrap(&config, &index).await {
                warn!(error = %e, "obligation index bootstrap failed");
            }
            if let Err(e) = liquidation::index::run(&config, &index).await {
                warn!(error = %e, "obligation index task exited");
            }
        });
    }
    // Oracle params refresh.
    {
        let params = params.clone();
        let config = config.clone();
        let client = client.clone();
        let metas = metas.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(10));
            loop {
                t.tick().await;
                if let Err(e) = source::refresh_params(&client, &config, &metas, &params).await {
                    tracing::debug!(error = %e, "liq params refresh failed");
                }
            }
        });
    }

    // Expose Prometheus /metrics (configurable; empty disables).
    if !config.metrics_addr.is_empty() {
        let addr = config.metrics_addr.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::metrics::serve(&addr).await {
                warn!(error = %e, "metrics server exited");
            }
        });
    }

    let lp = LiqParams {
        close_factor: config.liq_close_factor,
        liquidation_bonus: config.liq_bonus,
        flash_fee_bps: config.flash_fee_bps,
        gas_cost: config.gas_cost_estimate,
        min_profit: config.min_profit,
        candidate_fractions: vec![1.0, 0.75, 0.5, 0.25],
    };
    let src = LiquidationSource::new(index.clone(), params.clone(), lp, config.liq_health_margin);

    // Re-validate an obligation only when its repay sizing changes (avoids per-tick spam).
    let mut validated: HashMap<String, u64> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let pools = cache.snapshot();
        for opp in src.scan(&pools) {
            let Some(leg) = opp.liquidation.as_ref() else {
                continue;
            };
            if validated.get(&leg.obligation_id) == Some(&leg.repay_amount) {
                continue;
            }
            validated.insert(leg.obligation_id.clone(), leg.repay_amount);
            crate::metrics::inc_candidates(1);
            crate::metrics::inc_opportunity(crate::metrics::Opp::Liquidation);
            validate_and_log(&client, &objcache, config, &opp, &registry, &index, &params).await;
        }
    }
}

/// Validate one candidate end-to-end, logging each stage. Never submits.
#[allow(clippy::too_many_arguments)]
async fn validate_and_log(
    client: &sui_sdk::SuiClient,
    objcache: &ObjRefCache,
    config: &Config,
    opp: &Opportunity,
    registry: &LiveRegistry,
    index: &ObligationIndex,
    params: &SharedParams,
) {
    let Some(leg) = opp.liquidation.as_ref() else {
        return;
    };

    // 1. detected
    info!(
        stage = "detected",
        obligation = %leg.obligation_id,
        debt = %leg.debt_type,
        collateral = %leg.collateral_type,
        repay = leg.repay_amount,
        predicted_net = opp.net_profit,
        hops = opp.route.len(),
        "underwater liquidation candidate"
    );

    // 2. health factor (recomputed from the same index + params)
    match current_health_factor(index, params, &leg.obligation_id) {
        Some(hf) => {
            info!(stage = "health", obligation = %leg.obligation_id, health_factor = hf, "HF < 1 ⇒ liquidatable")
        }
        None => {
            info!(stage = "health", obligation = %leg.obligation_id, "HF unavailable (no priced debt)")
        }
    }

    // 3. oracle + build + dry-run (this is where Hermes is fetched, the VAA extracted, the
    //    PriceInfoObjects resolved, and the production PTB assembled + simulated).
    info!(
        stage = "oracle",
        obligation = %leg.obligation_id,
        debt_feed = config.pyth_feed_id(&leg.debt_type).unwrap_or("<unset>"),
        coll_feed = config.pyth_feed_id(&leg.collateral_type).unwrap_or("<unset>"),
        hermes = %config.hermes_url,
        "fetching accumulator + extracting VAA + building PTB"
    );

    let report = executor::validate_opportunity(client, objcache, config, opp, registry).await;

    if !report.built {
        warn!(
            stage = "ptb_build",
            obligation = %leg.obligation_id,
            reason = report.failure.as_deref().unwrap_or("unknown"),
            "PTB build / dry-run setup FAILED"
        );
        return;
    }
    info!(stage = "ptb_build", obligation = %leg.obligation_id, "production PTB assembled");

    // 4. dry-run result + predicted-vs-simulated
    if report.dry_run_success {
        let delta = report.simulated_net - report.predicted_net;
        info!(
            stage = "dry_run",
            obligation = %leg.obligation_id,
            success = true,
            gas = report.gas_used,
            predicted_net = report.predicted_net,
            simulated_net = report.simulated_net,
            delta,
            "DRY-RUN OK ✓ — pipeline validated (NOT submitted)"
        );
    } else {
        warn!(
            stage = "dry_run",
            obligation = %leg.obligation_id,
            success = false,
            reason = report.failure.as_deref().unwrap_or("revert"),
            "DRY-RUN FAILED — see reason"
        );
    }
}

/// Recompute the local health factor for an indexed obligation from the current params.
fn current_health_factor(
    index: &ObligationIndex,
    params: &SharedParams,
    obligation_id: &str,
) -> Option<f64> {
    let params = params.read().ok()?;
    let index = index.read().ok()?;
    let ob = index.get(obligation_id)?;
    health::health_factor(ob, &params)
}
