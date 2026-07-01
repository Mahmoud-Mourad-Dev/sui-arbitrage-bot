//! `arb-scanner` entrypoint.
//!
//! Default build: offline demo scan over a seeded graph (no network, no SDK).
//! `--features live`: hydrate from RPC, stream pool updates over WebSocket, scan
//! on every tick, and submit only profitable PTBs.

use anyhow::Result;
use tracing::info;

use arb_scanner::cache::ReserveCache;
use arb_scanner::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let config = Config::from_env()?;
    info!(rpc = %config.rpc_url, base = %config.base_token, "arb-scanner starting");

    let cache = ReserveCache::new();

    #[cfg(feature = "live")]
    run_live(config, cache).await?;

    #[cfg(not(feature = "live"))]
    run_demo(&config, &cache);

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Offline demonstration: seed a known triangular dislocation and scan it.
#[cfg(not(feature = "live"))]
fn run_demo(config: &Config, cache: &ReserveCache) {
    use arb_scanner::scanner::{self, ScanParams};
    use arb_scanner::types::{Dex, PoolState};

    let seed = [
        ("0xAB", "A", "B", 1_000_000_000u64, 1_000_000_000u64),
        ("0xBC", "B", "C", 1_000_000_000, 1_000_000_000),
        ("0xCA", "C", "A", 1_000_000_000, 2_000_000_000), // C ~ 2 A
    ];
    for (id, a, b, ra, rb) in seed {
        cache.upsert(PoolState::v2(id, Dex::AmmV2, a, b, ra, rb, 30));
    }

    let params = ScanParams {
        base_token: "A".into(),
        max_hops: config.max_hops,
        candidate_inputs: vec![1_000_000, 5_000_000, 10_000_000, 50_000_000, 100_000_000],
        gas_cost: config.gas_cost_estimate,
        flash_fee_bps: 0,
        min_profit: 1,
    };

    let pools = cache.snapshot();
    info!(
        pools = pools.len(),
        "demo scan (build with --features live for chain mode)"
    );
    match scanner::find_best(&pools, &params) {
        Some(opp) => info!(
            input = opp.input_amount,
            output = opp.output_amount,
            net_profit = opp.net_profit,
            hops = opp.route.len(),
            "opportunity found"
        ),
        None => info!("no opportunity in demo graph"),
    }
}

/// Live mode: bootstrap, stream updates, scan, and submit profitable PTBs.
#[cfg(feature = "live")]
async fn run_live(config: Config, cache: ReserveCache) -> Result<()> {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    use arb_scanner::objcache::ObjRefCache;
    use arb_scanner::risk::{RiskConfig, RiskGuard};
    use arb_scanner::scanner::{self, ScanParams};
    use arb_scanner::ws::LiveRegistry;
    use arb_scanner::{executor, ingest, ws};
    use sui_sdk::SuiClientBuilder;

    // Validate safety-critical invariants before spawning background tasks or
    // touching the signing path. Owned mode currently splits the SUI gas coin and
    // therefore cannot honestly support an arbitrary configured base token.
    if !config.sui_price_usd.is_finite() || config.sui_price_usd <= 0.0 {
        anyhow::bail!("ARB_SUI_PRICE_USD must be finite and > 0");
    }
    if config.max_quote_age_ms == 0 {
        anyhow::bail!("ARB_MAX_QUOTE_AGE_MS must be > 0");
    }
    if config.execution_mode == arb_scanner::config::ExecMode::Owned
        && config.base_token != "0x2::sui::SUI"
    {
        anyhow::bail!("owned execution currently requires ARB_BASE_TOKEN=0x2::sui::SUI");
    }
    if config.liq_enabled && config.submit_enabled {
        anyhow::bail!(
            "live liquidation submission is disabled until risk accounting is asset-aware"
        );
    }

    let cache = Arc::new(cache);
    // On-chain object coordinates (pool refs) the quoter/PTB builder need.
    let registry: LiveRegistry = Arc::new(RwLock::new(HashMap::new()));

    // Risk guard: kill switch / daily-loss cap / pool blacklist. Shared across ticks.
    let mut guard = RiskGuard::new(RiskConfig::new(
        config.max_daily_loss_usd,
        config.kill_switch,
        config.pool_blacklist.clone(),
    ));

    // Base-token USD valuation for the risk guard. SUI = 9 decimals. Until a live
    // oracle is wired, use the explicitly configured operator price consistently
    // (the owned-mode USD min-profit conversion uses the same value).
    let base_decimals: u32 = 9;
    let base_price_usd: f64 = config.sui_price_usd;

    // Fail closed on contradictory funding configuration. `execution_mode=flash`
    // must never silently borrow when the operator disabled flash loans.
    if config.execution_mode == arb_scanner::config::ExecMode::Flash && !config.flash_enabled {
        anyhow::bail!("ARB_EXECUTION_MODE=flash requires ARB_FLASH_ENABLED=true");
    }

    // Shared, long-lived RPC client for the executor hot path — built ONCE (no
    // per-candidate rebuild), plus a shared-object-ref cache so the PTB builder never
    // re-fetches static object versions per attempt.
    let client = Arc::new(SuiClientBuilder::default().build(&config.rpc_url).await?);
    let objcache = ObjRefCache::new();

    // 1+2. Populate + keep the cache/registry hot. With the indexer enabled (default), pools
    //      are auto-discovered on-chain — ARB_TRACKED_POOLS is ignored. Otherwise, hydrate
    //      the manual tracked-pool set and refresh it (poll or checkpoint mode).
    if config.indexer_enabled {
        let cache = cache.clone();
        let registry = registry.clone();
        let config = config.clone();
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = arb_scanner::indexer::run(&config, &cache, &registry, &client).await {
                tracing::error!(error = %e, "indexer task exited");
            }
        });
    } else {
        ws::bootstrap_pools(&config, &cache, &registry).await?;
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
                tracing::error!(error = %e, "ingest task exited");
            }
        });
    }

    // 2b. Liquidation source (optional): an obligation index + oracle-priced asset params,
    //     both kept fresh by background tasks (like pool ingestion), emitted into the SAME
    //     executor + risk guard as arb. Gated by ARB_LIQ_ENABLED; submit still gated by
    //     ARB_SUBMIT_ENABLED (paper by default).
    let liq_source = if config.liq_enabled {
        use arb_scanner::liquidation::detect::LiqParams;
        use arb_scanner::liquidation::source::{self, LiquidationSource, SharedParams};
        use arb_scanner::liquidation::{self, ObligationIndex};

        let index: ObligationIndex = Arc::new(RwLock::new(HashMap::new()));
        let params: SharedParams = Arc::new(RwLock::new(HashMap::new()));
        let metas = source::parse_asset_meta(&config.liq_assets);

        // (a) obligation index: bootstrap once, then RPC reconcile.
        {
            let index = index.clone();
            let config = config.clone();
            tokio::spawn(async move {
                if let Err(e) = liquidation::index::bootstrap(&config, &index).await {
                    tracing::warn!(error = %e, "liq index bootstrap failed");
                }
                if let Err(e) = liquidation::index::run(&config, &index).await {
                    tracing::error!(error = %e, "liq index task exited");
                }
            });
        }
        // (b) params: refresh asset prices from the protocol oracle.
        {
            let params = params.clone();
            let config = config.clone();
            let client = client.clone();
            let metas = metas.clone();
            tokio::spawn(async move {
                let mut t = tokio::time::interval(Duration::from_secs(10));
                loop {
                    t.tick().await;
                    if let Err(e) = source::refresh_params(&client, &config, &metas, &params).await
                    {
                        tracing::debug!(error = %e, "liq params refresh failed");
                    }
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
        tracing::info!(assets = metas.len(), "liquidation source enabled");
        Some(LiquidationSource::new(
            index,
            params,
            lp,
            config.liq_health_margin,
        ))
    } else {
        None
    };

    // Observability: expose Prometheus /metrics (configurable; empty disables).
    if !config.metrics_addr.is_empty() {
        let addr = config.metrics_addr.clone();
        tokio::spawn(async move {
            if let Err(e) = arb_scanner::metrics::serve(&addr).await {
                tracing::warn!(error = %e, "metrics server exited");
            }
        });
    }

    // 3. Scan + (gated) submit loop.
    let mut tick = tokio::time::interval(Duration::from_millis(config.poll_interval_ms));
    let mut ticks: u64 = 0;
    // Heartbeat every ~30s so the operator can see it's alive between (rare) candidates.
    let heartbeat = (30_000 / config.poll_interval_ms.max(1)).max(1);
    loop {
        tick.tick().await;
        ticks += 1;
        let pools = cache.snapshot();
        arb_scanner::metrics::set_cache_size(pools.len());
        // In indexer mode the indexer owns this gauge. Overwriting it here with the
        // (intentionally empty) manual list made the production dashboard report 0.
        if !config.indexer_enabled {
            arb_scanner::metrics::set_tracked_pools(config.tracked_pools.len());
        }
        let params = ScanParams {
            base_token: config.base_token.clone(),
            max_hops: config.max_hops,
            candidate_inputs: config.candidate_inputs.clone(),
            gas_cost: config.gas_cost_estimate,
            flash_fee_bps: config.flash_fee_bps,
            min_profit: config.effective_min_profit_mist(),
        };
        // Merge every source's opportunities into the one pipeline; execute the best net.
        let mut candidates: Vec<_> = {
            use arb_scanner::metrics::{stage_timer, Stage};
            let _t = stage_timer(Stage::Scanner);
            scanner::find_best(&pools, &params).into_iter().collect()
        };
        if let Some(src) = &liq_source {
            use arb_scanner::strategy::OpportunitySource;
            candidates.extend(src.scan(&pools));
        }
        if !candidates.is_empty() {
            arb_scanner::metrics::inc_candidates(candidates.len() as u64);
            for c in &candidates {
                arb_scanner::metrics::inc_opportunity(match c.kind {
                    arb_scanner::scanner::OppKind::Arb => arb_scanner::metrics::Opp::Arb,
                    arb_scanner::scanner::OppKind::Liquidation => {
                        arb_scanner::metrics::Opp::Liquidation
                    }
                    arb_scanner::scanner::OppKind::Backrun => arb_scanner::metrics::Opp::Backrun,
                });
            }
        }
        let found = candidates.into_iter().max_by_key(|o| o.net_profit);
        if ticks.is_multiple_of(heartbeat) {
            info!(
                ticks,
                pools = pools.len(),
                candidate = found.is_some(),
                "alive (scanning; logs a candidate only when one clears min_profit)"
            );
        }
        if let Some(opp) = found {
            info!(
                kind = ?opp.kind,
                input = opp.input_amount,
                est_out = opp.output_amount,
                est_net = opp.net_profit,
                hops = opp.route.len(),
                "candidate — re-quoting + dry-running"
            );
            let _t = arb_scanner::metrics::stage_timer(arb_scanner::metrics::Stage::Executor);
            // Owned-capital mode (arb only): re-size the route from wallet SUI and dry-run a
            // size ladder; flash mode (and liquidations) keep the existing path.
            let owned = config.execution_mode == arb_scanner::config::ExecMode::Owned
                && opp.kind == arb_scanner::scanner::OppKind::Arb;
            let result = if owned {
                executor::try_execute_owned_sized(
                    &client,
                    &objcache,
                    &config,
                    &opp.route,
                    &pools,
                    &registry,
                    &mut guard,
                    base_decimals,
                    base_price_usd,
                )
                .await
                .map(|_| ())
            } else {
                executor::try_execute(
                    &client,
                    &objcache,
                    &config,
                    &opp,
                    &registry,
                    &mut guard,
                    base_decimals,
                    base_price_usd,
                )
                .await
                .map(|_| ())
            };
            if let Err(e) = result {
                tracing::warn!(error = %e, "execution failed");
            }
        }
    }
}
