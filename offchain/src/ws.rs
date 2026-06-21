//! Live pool ingestion (feature = "live").
//!
//! Two responsibilities:
//!   * `bootstrap_pools` — one-shot RPC hydrate of every tracked pool.
//!   * `run` — subscribe to `amm_v2` (and adapter) events and keep reserves hot.
//!
//! This is the integration seam: the exact event/field parsing depends on the
//! deployed Move structs and the pinned SDK. The shape is faithful; the TODOs
//! mark where to plug concrete object/event decoding.

use std::sync::Arc;

use anyhow::Result;

use crate::cache::ReserveCache;
use crate::config::Config;

/// Hydrate the cache with the current state of all tracked pools via JSON-RPC.
pub async fn bootstrap_pools(config: &Config, cache: &Arc<ReserveCache>) -> Result<()> {
    use sui_sdk::SuiClientBuilder;

    let client = SuiClientBuilder::default().build(&config.rpc_url).await?;

    // TODO: source pool object ids (config list or an on-chain registry), then
    // client.read_api().multi_get_object_with_options(ids, content) and decode
    // each Move object's fields into PoolState before cache.upsert(state).
    let _ = (&client, cache);
    tracing::info!("bootstrap_pools: hydrate tracked pools here");
    Ok(())
}

/// Subscribe to pool events and apply reserve deltas to the cache.
pub async fn run(config: &Config, cache: &Arc<ReserveCache>) -> Result<()> {
    use futures_util::StreamExt;
    use sui_json_rpc_types::EventFilter;
    use sui_sdk::SuiClientBuilder;

    let client = SuiClientBuilder::default()
        .ws_url(&config.ws_url)
        .build(&config.rpc_url)
        .await?;

    let filter = EventFilter::MoveModule {
        package: config.package_id.parse()?,
        module: "amm_v2".parse()?,
    };

    let mut stream = client.event_api().subscribe_event(filter).await?;
    tracing::info!("ws: subscribed to amm_v2 events");

    while let Some(event) = stream.next().await {
        let event = event?;
        // TODO: match event.type_, decode event.parsed_json into
        // (pool_id, reserve_a, reserve_b) and apply:
        if let Some((id, ra, rb)) = decode_reserves(&event) {
            if !cache.update_reserves(&id, ra, rb) {
                tracing::debug!(%id, "reserve update for untracked pool");
            }
        }
    }
    Ok(())
}

/// Decode a pool event into a reserve update. Placeholder for the deployed schema.
fn decode_reserves(_event: &sui_json_rpc_types::SuiEvent) -> Option<(String, u64, u64)> {
    None
}
