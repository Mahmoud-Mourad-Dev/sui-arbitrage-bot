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

    use arb_scanner::risk::{RiskConfig, RiskGuard};
    use arb_scanner::scanner::{self, ScanParams};
    use arb_scanner::ws::LiveRegistry;
    use arb_scanner::{executor, ws};

    let cache = Arc::new(cache);
    // On-chain object coordinates (pool refs) the quoter/PTB builder need.
    let registry: LiveRegistry = Arc::new(RwLock::new(HashMap::new()));

    // Risk guard: kill switch / daily-loss cap / pool blacklist. Shared across ticks.
    let mut guard = RiskGuard::new(RiskConfig::new(
        config.max_daily_loss_usd,
        config.kill_switch,
        config.pool_blacklist.clone(),
    ));

    // Base-token USD valuation for the risk guard. SUI = 9 decimals; the price is a
    // live input — until a price feed is wired (Phase 6), pass 1.0 so the guard gates
    // in base units rather than USD.
    let base_decimals: u32 = 9;
    let base_price_usd: f64 = 1.0;

    // 1. Hydrate the cache + registry with current pool state.
    ws::bootstrap_pools(&config, &cache, &registry).await?;

    // 2. Keep the cache + registry hot from the event stream.
    {
        let cache = cache.clone();
        let registry = registry.clone();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = ws::run(&config, &cache, &registry).await {
                tracing::error!(error = %e, "ws task exited");
            }
        });
    }

    // 3. Scan + (gated) submit loop.
    let mut tick = tokio::time::interval(Duration::from_millis(config.poll_interval_ms));
    loop {
        tick.tick().await;
        let pools = cache.snapshot();
        let params = ScanParams {
            base_token: config.base_token.clone(),
            max_hops: config.max_hops,
            candidate_inputs: config.candidate_inputs.clone(),
            gas_cost: config.gas_cost_estimate,
            flash_fee_bps: config.flash_fee_bps,
            min_profit: config.min_profit,
        };
        if let Some(opp) = scanner::find_best(&pools, &params) {
            if let Err(e) = executor::try_execute(
                &config,
                &opp,
                &registry,
                &mut guard,
                base_decimals,
                base_price_usd,
            )
            .await
            {
                tracing::warn!(error = %e, "execution failed");
            }
        }
    }
}
