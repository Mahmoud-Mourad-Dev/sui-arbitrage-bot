//! Automated multi-DEX pool indexer (feature = "live").
//!
//! Replaces the manual `ARB_TRACKED_POOLS` list with on-chain discovery: it pages each
//! DEX's pool-creation events to find every pool, decodes the live objects into the unified
//! [`PoolState`]/[`LivePoolRef`] the scanner + executor already use, filters to the
//! liquid/valid set, and keeps the shared cache hot — all on a background task that never
//! blocks the scanner loop. It reuses the shared `Arc<SuiClient>`.
//!
//! Discovery is verified against mainnet:
//!
//! - Cetus: `0x1eabed72…::factory::CreatePoolEvent` (fields: pool_id, coin_type_a/b).
//! - Turbos: `0x91bfbc38…::pool_factory::PoolCreatedEvent` (pool id in field `pool`).
//!
//! DeepBook is an order-book (not the CLMM/V2 quote model), so its adapter discovers pools
//! but is marked non-quotable and is **off by default**.
//!
//! Modes: `poll` (RPC, implemented here) and `checkpoint` (future fullnode stream — the
//! `sync` seam below is where it plugs in, mirroring `ingest::run_checkpoint_diff`).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use sui_json_rpc_types::{SuiObjectDataOptions, SuiObjectResponse};
use sui_sdk::SuiClient;
use sui_types::base_types::ObjectID;

use crate::cache::ReserveCache;
use crate::clmm::TickBoundary;
use crate::config::Config;
use crate::metrics::{self, IndexDex, Rpc};
use crate::quoter::LivePoolRef;
use crate::types::{Dex, PoolKind, PoolState};
use crate::ws::{self, LiveRegistry};

/// Per-pool cache of decoded ticks (pool id → windowed ticks). Populated on the discovery
/// cycle and reused on the fast refresh so tick reads are infrequent, not per-tick.
type TickCache = Arc<RwLock<HashMap<String, Vec<TickBoundary>>>>;

/// Where to read a pool's initialized ticks (when tick loading is on). Cetus reads a separate
/// SkipList keyed by score; Turbos reads dynamic fields on the pool itself, keyed by tick index
/// (sqrt_price derived from the pool's current price).
#[derive(Clone, Copy)]
enum TickSrc {
    Cetus {
        skiplist: ObjectID,
        score: u64,
    },
    Turbos {
        pool: ObjectID,
        sqrt: u128,
        tick: i32,
    },
}

/// A kept pool pending tick injection: state, live ref, liquidity, and (when tick loading is
/// on) its tick source for windowed loading.
type KeptPool = (PoolState, LivePoolRef, u128, Option<TickSrc>);

/// The pool-creation event that announces new pools for a DEX.
pub struct EventSpec {
    /// Defining package of the event (original publish).
    pub package: &'static str,
    /// Module that emits the creation event.
    pub module: &'static str,
    /// `parsed_json` field holding the new pool's object id.
    pub pool_id_field: &'static str,
}

/// A pluggable DEX integration. `discover` (driven by [`EventSpec`]) + `decode` (object →
/// unified pool) + `normalize` (filter/keep decision) are the three responsibilities.
pub trait PoolAdapter: Send + Sync {
    fn dex(&self) -> Dex;
    fn label(&self) -> &'static str;
    /// Descriptor that drives discovery (the creation event to page through).
    fn discover_spec(&self) -> EventSpec;
    /// Decode a pool object into the unified `(PoolState, LivePoolRef)`.
    fn decode(&self, obj: &SuiObjectResponse) -> Result<(PoolState, LivePoolRef)>;
    /// Keep decision (normalize): default accepts; can be overridden per venue.
    fn normalize(&self, _state: &PoolState) -> bool {
        true
    }
    /// Whether the scanner can quote this venue. Non-quotable venues are discovered + counted
    /// but not fed into the cache (the scanner has no model for them yet).
    fn quotable(&self) -> bool {
        true
    }
}

/// Cetus CLMM — `factory::CreatePoolEvent`; decode reuses the venue-aware `ws::decode_pool`.
pub struct CetusAdapter;
impl PoolAdapter for CetusAdapter {
    fn dex(&self) -> Dex {
        Dex::Cetus
    }
    fn label(&self) -> &'static str {
        "cetus"
    }
    fn discover_spec(&self) -> EventSpec {
        EventSpec {
            package: "0x1eabed72c53feb3805120a081dc15963c204dc8d091542592abaf7a35689b2fb",
            module: "factory",
            pool_id_field: "pool_id",
        }
    }
    fn decode(&self, obj: &SuiObjectResponse) -> Result<(PoolState, LivePoolRef)> {
        ws::decode_pool(Dex::Cetus, obj)
    }
}

/// Turbos CLMM — `pool_factory::PoolCreatedEvent` (pool id in field `pool`; coin types come
/// from decoding the object).
pub struct TurbosAdapter;
impl PoolAdapter for TurbosAdapter {
    fn dex(&self) -> Dex {
        Dex::Turbos
    }
    fn label(&self) -> &'static str {
        "turbos"
    }
    fn discover_spec(&self) -> EventSpec {
        EventSpec {
            package: "0x91bfbc386a41afcfd9b2533058d7e915a1d3829089cc268ff4333d54d6339ca1",
            module: "pool_factory",
            pool_id_field: "pool",
        }
    }
    fn decode(&self, obj: &SuiObjectResponse) -> Result<(PoolState, LivePoolRef)> {
        ws::decode_pool(Dex::Turbos, obj)
    }
}

/// DeepBook — order-book; discovery scaffold only (non-quotable until an order-book quote
/// model exists). VERIFICATION STATUS: event spec not yet confirmed on mainnet; off by default.
pub struct DeepBookAdapter;
impl PoolAdapter for DeepBookAdapter {
    fn dex(&self) -> Dex {
        Dex::Cetus // placeholder; DeepBook is non-quotable so this is never fed to the cache
    }
    fn label(&self) -> &'static str {
        "deepbook"
    }
    fn discover_spec(&self) -> EventSpec {
        EventSpec {
            package: "0x000000000000000000000000000000000000000000000000000000000000deeb",
            module: "pool",
            pool_id_field: "pool_id",
        }
    }
    fn decode(&self, _obj: &SuiObjectResponse) -> Result<(PoolState, LivePoolRef)> {
        anyhow::bail!("deepbook order-book decode not implemented")
    }
    fn quotable(&self) -> bool {
        false
    }
}

/// Build the configured adapters. Unknown names are skipped with a warning.
#[must_use]
pub fn build_adapters(dexes: &[String]) -> Vec<Box<dyn PoolAdapter>> {
    let mut out: Vec<Box<dyn PoolAdapter>> = Vec::new();
    for d in dexes {
        match d.trim().to_ascii_lowercase().as_str() {
            "cetus" => out.push(Box::new(CetusAdapter)),
            "turbos" => out.push(Box::new(TurbosAdapter)),
            "deepbook" => out.push(Box::new(DeepBookAdapter)),
            other => tracing::warn!(dex = other, "indexer: unknown DEX, skipping"),
        }
    }
    out
}

fn metric_dex(label: &str) -> IndexDex {
    match label {
        "cetus" => IndexDex::Cetus,
        "turbos" => IndexDex::Turbos,
        "deepbook" => IndexDex::DeepBook,
        _ => IndexDex::Other,
    }
}

/// Liquidity proxy for filtering/ranking: CLMM `liquidity`, or `min(reserves)` for V2.
fn liquidity_of(state: &PoolState) -> u128 {
    match &state.kind {
        PoolKind::Clmm(c) => c.liquidity,
        PoolKind::V2 {
            reserve_a,
            reserve_b,
            ..
        } => u128::from(*reserve_a).min(u128::from(*reserve_b)),
    }
}

fn pair_allowed(state: &PoolState, quote_tokens: &[String]) -> bool {
    quote_tokens.is_empty()
        || quote_tokens
            .iter()
            .any(|t| t == &state.token_a || t == &state.token_b)
}

/// Replace a CLMM pool's tick set in place (no-op for V2).
fn inject_ticks(state: &mut PoolState, ticks: Vec<crate::clmm::TickBoundary>) {
    if let PoolKind::Clmm(c) = &mut state.kind {
        c.ticks = ticks;
    }
}

/// Load a pool's windowed ticks from the correct on-chain source (Cetus SkipList vs Turbos
/// pool dynamic fields). Returns the engine's `TickBoundary`s sorted by `sqrt_price`.
async fn load_ticks_for(
    client: &SuiClient,
    src: TickSrc,
    window: usize,
) -> Result<Vec<TickBoundary>> {
    match src {
        TickSrc::Cetus { skiplist, score } => {
            crate::cetus_ticks::load_window(client, skiplist, score, window).await
        }
        TickSrc::Turbos { pool, sqrt, tick } => {
            crate::turbos_ticks::load_window(client, pool, sqrt, tick, window).await
        }
    }
}

/// Current sqrt-price of a CLMM pool (0 for V2), for diagnostics.
fn clmm_sqrt(state: &PoolState) -> u128 {
    match &state.kind {
        PoolKind::Clmm(c) => c.sqrt_price,
        PoolKind::V2 { .. } => 0,
    }
}

/// Diagnostic: quote a representative large input with the empty single-range approximation
/// vs. with the loaded ticks — shows the over-quote shrinking to reality.
fn tick_quote_compare(state: &PoolState, ticks: &[crate::clmm::TickBoundary]) -> (u64, u64) {
    let PoolKind::Clmm(base) = &state.kind else {
        return (0, 0);
    };
    const SAMPLE_IN: u64 = 100_000_000_000; // 1e11 raw units — large enough to expose over-quote
    let empty = base.clone(); // ticks are still empty at this point
    let mut with_ticks = base.clone();
    with_ticks.ticks = ticks.to_vec();
    let before = crate::clmm::quote_exact_in(&empty, SAMPLE_IN, true).unwrap_or(0);
    let after = crate::clmm::quote_exact_in(&with_ticks, SAMPLE_IN, true).unwrap_or(0);
    (before, after)
}

/// Hard cap on ids held per DEX (bounds memory for very large venues).
const MAX_DISCOVER: usize = 5_000;

/// Page a DEX's creation events into pool object ids (deduped, capped).
///
/// Uses `MoveEventModule` (events whose *type* is defined in the factory module) — NOT
/// `MoveModule` (events *emitted by* that module). Pool creations are typically driven by a
/// router/creator that calls the factory, so `MoveModule{factory}` matches nothing; the
/// event type is still `factory::CreatePoolEvent`, which `MoveEventModule` matches.
async fn discover_ids(client: &SuiClient, spec: &EventSpec) -> Result<Vec<String>> {
    use sui_json_rpc_types::EventFilter;
    let filter = EventFilter::MoveEventModule {
        package: spec.package.parse()?,
        module: spec.module.parse()?,
    };
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    loop {
        let page = match metrics::time_rpc(
            Rpc::QueryEvents,
            client
                .event_api()
                .query_events(filter.clone(), cursor, Some(200), false),
        )
        .await
        {
            Ok(p) => {
                metrics::set_rpc_up(true);
                p
            }
            Err(e) => {
                metrics::set_rpc_up(false);
                return Err(e.into());
            }
        };
        for ev in &page.data {
            if let Some(id) = ev
                .parsed_json
                .get(spec.pool_id_field)
                .and_then(|v| v.as_str())
            {
                if seen.insert(id.to_string()) {
                    ids.push(id.to_string());
                }
            }
        }
        if !page.has_next_page || ids.len() >= MAX_DISCOVER {
            break;
        }
        cursor = page.next_cursor;
    }
    Ok(ids)
}

/// Decode + filter `ids` for one adapter, upsert the kept pools into the cache + registry,
/// and return the kept ids (highest-liquidity first, capped at `cap`). Batched object reads.
#[allow(clippy::too_many_arguments)]
async fn sync_pools(
    client: &SuiClient,
    adapter: &dyn PoolAdapter,
    ids: &[String],
    config: &Config,
    cache: &Arc<ReserveCache>,
    registry: &LiveRegistry,
    cap: usize,
    tick_cache: &TickCache,
    refetch_ticks: bool,
) -> Vec<String> {
    use futures_util::stream::{self, StreamExt};

    let opts = SuiObjectDataOptions::new()
        .with_content()
        .with_type()
        .with_owner();
    // 4th field: for a Cetus pool (when tick loading is on), its tick-SkipList UID + the
    // current skip-list score (to center the window). `None` for non-Cetus / loading off.
    let mut kept: Vec<KeptPool> = Vec::new();

    // Fetch object batches with BOUNDED concurrency so bootstrap (thousands of pools) takes
    // seconds, not minutes — without flooding the RPC. Decode happens after each batch lands.
    let chunks: Vec<Vec<ObjectID>> = ids
        .chunks(50)
        .map(|c| c.iter().filter_map(|s| s.parse().ok()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .collect();
    let batches = stream::iter(chunks)
        .map(|oids| {
            let opts = opts.clone();
            async move {
                metrics::time_rpc(
                    Rpc::MultiGetObject,
                    client.read_api().multi_get_object_with_options(oids, opts),
                )
                .await
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;

    for res in batches {
        match res {
            Ok(objs) => {
                metrics::set_rpc_up(true);
                for obj in &objs {
                    match adapter.decode(obj) {
                        Ok((state, lref)) => {
                            let liq = liquidity_of(&state);
                            if liq >= config.indexer_min_liquidity
                                && pair_allowed(&state, &config.indexer_quote_tokens)
                                && adapter.normalize(&state)
                            {
                                let skid =
                                    if config.indexer_load_ticks {
                                        match adapter.dex() {
                                            Dex::Cetus => crate::cetus_ticks::skiplist_id(obj)
                                                .zip(crate::cetus_ticks::current_score(obj))
                                                .map(|(skiplist, score)| TickSrc::Cetus {
                                                    skiplist,
                                                    score,
                                                }),
                                            Dex::Turbos => crate::turbos_ticks::current_tick(obj)
                                                .map(|tick| TickSrc::Turbos {
                                                    pool: lref.pool_id,
                                                    sqrt: clmm_sqrt(&state),
                                                    tick,
                                                }),
                                            Dex::AmmV2 => None,
                                        }
                                    } else {
                                        None
                                    };
                                kept.push((state, lref, liq, skid));
                            }
                        }
                        Err(e) => tracing::debug!(dex = adapter.label(), "decode skipped: {e}"),
                    }
                }
            }
            Err(e) => {
                metrics::set_rpc_up(false);
                tracing::warn!(dex = adapter.label(), "indexer batch read failed: {e}");
            }
        }
    }

    kept.sort_by_key(|k| std::cmp::Reverse(k.2));
    kept.truncate(cap);
    if !kept.is_empty() {
        let n = kept.len();
        tracing::debug!(
            dex = adapter.label(),
            kept = n,
            liq_max = kept.first().map(|k| k.2).unwrap_or(0),
            liq_p50 = kept.get(n / 2).map(|k| k.2).unwrap_or(0),
            liq_min = kept.last().map(|k| k.2).unwrap_or(0),
            "kept-pool liquidity distribution"
        );
    }

    // Tick ingestion (Cetus only): replace the empty single-range approximation with the
    // pool's real initialized ticks near the current price. RELIABILITY:
    //   * cached per pool — the fast refresh REUSES cache (no RPC); only the discovery cycle
    //     (`refetch_ticks`) re-fetches, so tick reads are infrequent;
    //   * windowed — only the ticks a realistic swap can cross are fetched;
    //   * exponential backoff on transient RPC error (429), then fall back to STALE cache;
    //   * a pool is DROPPED only if it has no cached ticks AND the load fails/empties — we
    //     never fall back to empty ticks (which silently over-quote). Turbos: unchanged.
    let kept: Vec<(PoolState, LivePoolRef, u128)> = if config.indexer_load_ticks {
        use futures_util::stream::{self, StreamExt};
        let window = config.indexer_max_ticks;
        stream::iter(kept)
            .map(|(mut state, lref, liq, skid)| {
                let tick_cache = Arc::clone(tick_cache);
                async move {
                    let Some(src) = skid else {
                        return Some((state, lref, liq)); // V2 / no tick source: unchanged
                    };
                    let cached = tick_cache.read().expect("tick cache poisoned").get(&state.id).cloned();

                    // Fast refresh with a warm cache: reuse, no RPC.
                    if !refetch_ticks {
                        if let Some(ticks) = cached {
                            inject_ticks(&mut state, ticks);
                            return Some((state, lref, liq));
                        }
                    }

                    // (Re)fetch the window (Cetus or Turbos), with exponential backoff on
                    // transient RPC errors (chiefly `429` during the bootstrap burst). Per-pool
                    // jitter desyncs retries so they don't all hammer the RPC on the same beat.
                    let mut res = load_ticks_for(client, src, window).await;
                    let jitter = state.id.bytes().map(u64::from).sum::<u64>() % 200;
                    let mut delay = 250u64;
                    for _ in 0..3 {
                        if res.is_ok() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                        res = load_ticks_for(client, src, window).await;
                        delay *= 3;
                    }
                    match res {
                        Ok(ticks) if !ticks.is_empty() => {
                            let n = ticks.len();
                            let (q_before, q_after) = tick_quote_compare(&state, &ticks);
                            tick_cache
                                .write()
                                .expect("tick cache poisoned")
                                .insert(state.id.clone(), ticks.clone());
                            let dex = state.dex;
                            inject_ticks(&mut state, ticks);
                            tracing::info!(
                                pool = %state.id, dex = ?dex, cur_sqrt = clmm_sqrt(&state), ticks = n,
                                q_before, q_after, crosses = (q_before != q_after),
                                "ticks loaded"
                            );
                            Some((state, lref, liq))
                        }
                        // Load failed/empty: reuse stale cache if present, else drop.
                        other => {
                            if let Some(ticks) = cached {
                                inject_ticks(&mut state, ticks);
                                tracing::debug!(pool = %state.id, "ticks: reused stale cache (refetch failed)");
                                Some((state, lref, liq))
                            } else {
                                match other {
                                    Err(e) => tracing::warn!(pool = %state.id, dex = ?state.dex, "pool DROPPED: tick load failed: {e}"),
                                    _ => tracing::warn!(pool = %state.id, dex = ?state.dex, "pool DROPPED: 0 ticks"),
                                }
                                None
                            }
                        }
                    }
                }
            })
            // Modest concurrency: the bootstrap fans out over all Cetus pools at once; too
            // many parallel tick loads trip RPC rate limits (429). Cached afterwards.
            .buffer_unordered(3)
            .collect::<Vec<Option<_>>>()
            .await
            .into_iter()
            .flatten()
            .collect()
    } else {
        kept.into_iter().map(|(s, l, q, _)| (s, l, q)).collect()
    };

    let mut active = Vec::with_capacity(kept.len());
    for (state, lref, _) in kept {
        let id = state.id.clone();
        registry
            .write()
            .expect("registry poisoned")
            .insert(id.clone(), lref);
        cache.upsert(state);
        active.push(id);
    }
    metrics::set_dex_pools(metric_dex(adapter.label()), active.len());
    active
}

/// Run the indexer: bootstrap discovery, then refresh the active set on `indexer_refresh_secs`
/// and re-discover new listings on `indexer_discovery_secs`. Never returns under normal
/// operation. Reuses the shared client; updates the same cache + registry the scanner reads.
pub async fn run(
    config: &Config,
    cache: &Arc<ReserveCache>,
    registry: &LiveRegistry,
    client: &SuiClient,
) -> Result<()> {
    let adapters = build_adapters(&config.indexer_dexes);
    if adapters.is_empty() {
        tracing::warn!("indexer: no adapters configured (ARB_INDEXER_DEXES)");
        return Ok(());
    }
    tracing::info!(
        dexes = ?config.indexer_dexes,
        max_pools = config.indexer_max_pools,
        min_liquidity = config.indexer_min_liquidity,
        "indexer starting (auto pool discovery)"
    );

    // Per-adapter candidate id sets + active id sets.
    let mut candidates: Vec<Vec<String>> = vec![Vec::new(); adapters.len()];
    let mut active: Vec<Vec<String>> = vec![Vec::new(); adapters.len()];
    // Shared Cetus tick cache: filled on discovery, reused on the fast refresh.
    let tick_cache: TickCache = Arc::new(RwLock::new(HashMap::new()));

    // Bootstrap: discover + build the active set for every adapter.
    for (i, a) in adapters.iter().enumerate() {
        match discover_ids(client, &a.discover_spec()).await {
            Ok(ids) => {
                tracing::info!(
                    dex = a.label(),
                    discovered = ids.len(),
                    "indexer discovered"
                );
                candidates[i] = ids;
            }
            Err(e) => tracing::warn!(dex = a.label(), "discover failed: {e}"),
        }
        if a.quotable() {
            active[i] = sync_pools(
                client,
                a.as_ref(),
                &candidates[i],
                config,
                cache,
                registry,
                config.indexer_max_pools,
                &tick_cache,
                true, // bootstrap: fetch ticks
            )
            .await;
        } else {
            metrics::set_dex_pools(metric_dex(a.label()), candidates[i].len());
        }
    }
    report_totals(&candidates, &active, cache);

    let mut tick = tokio::time::interval(Duration::from_secs(config.indexer_refresh_secs.max(2)));
    let mut last_discovery = Instant::now();
    loop {
        tick.tick().await;
        let _t = metrics::stage_timer(metrics::Stage::Ingestion);

        // Periodic re-discovery (new listings) → rebuild the active set from candidates.
        if last_discovery.elapsed() >= Duration::from_secs(config.indexer_discovery_secs) {
            for (i, a) in adapters.iter().enumerate() {
                if let Ok(ids) = discover_ids(client, &a.discover_spec()).await {
                    candidates[i] = ids;
                }
                if a.quotable() {
                    active[i] = sync_pools(
                        client,
                        a.as_ref(),
                        &candidates[i],
                        config,
                        cache,
                        registry,
                        config.indexer_max_pools,
                        &tick_cache,
                        true, // discovery: refetch ticks
                    )
                    .await;
                }
            }
            last_discovery = Instant::now();
        } else {
            // Fast path: refresh only the active set's state.
            for (i, a) in adapters.iter().enumerate() {
                if a.quotable() && !active[i].is_empty() {
                    active[i] = sync_pools(
                        client,
                        a.as_ref(),
                        &active[i],
                        config,
                        cache,
                        registry,
                        config.indexer_max_pools,
                        &tick_cache,
                        false, // fast refresh: reuse cached ticks
                    )
                    .await;
                }
            }
        }
        report_totals(&candidates, &active, cache);
    }
}

fn report_totals(candidates: &[Vec<String>], active: &[Vec<String>], cache: &Arc<ReserveCache>) {
    let discovered: usize = candidates.iter().map(Vec::len).sum();
    let kept: usize = active.iter().map(Vec::len).sum();
    metrics::set_discovered_total(discovered);
    metrics::set_tracked_pools(kept);
    metrics::set_cache_size(cache.len());
    metrics::inc_pools_updated(kept as u64);
    tracing::debug!(discovered, active = kept, "indexer refresh");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_adapters_maps_names() {
        let a = build_adapters(&[
            "cetus".into(),
            "TURBOS".into(),
            "deepbook".into(),
            "nope".into(),
        ]);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].label(), "cetus");
        assert!(a[0].quotable());
        assert!(!a[2].quotable()); // deepbook
    }

    #[test]
    fn pair_allowlist_filters() {
        let p = PoolState::v2("0x1", Dex::Cetus, "0x2::sui::SUI", "0xUSDC", 100, 100, 30);
        assert!(pair_allowed(&p, &[])); // empty = allow all
        assert!(pair_allowed(&p, &["0x2::sui::SUI".to_string()]));
        assert!(!pair_allowed(&p, &["0xOTHER".to_string()]));
    }

    #[test]
    fn liquidity_proxy() {
        let v2 = PoolState::v2("0x1", Dex::Cetus, "A", "B", 500, 900, 30);
        assert_eq!(liquidity_of(&v2), 500);
    }
}
