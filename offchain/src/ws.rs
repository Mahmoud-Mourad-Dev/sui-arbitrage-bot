//! Live pool ingestion (feature = "live").
//!
//! CLMM pools do **not** emit simple reserve deltas, so (per the accepted
//! consolidation plan, Decision 2) ingestion is **event-triggered object re-read**:
//!   * `bootstrap_pools` — `multi_get_object_with_options` over the tracked pools,
//!     decode each into a CLMM `PoolState` (for the scanner) + a `LivePoolRef` (for
//!     the quoter/PTB), keyed by object version.
//!   * `run` — subscribe to each venue's swap / liquidity events; on an event for a
//!     tracked pool, re-read that pool object and upsert the fresh snapshot.
//!
//! Stage-1 scanning uses the pool's current `sqrt_price`/`liquidity` as a single
//! active range (cheap, approximate); the authoritative `quoter` (stage 2) reads full
//! on-chain state incl. ticks before anything is acted on, so ingestion does not need
//! the whole tick array on the hot path.
//!
//! VERIFICATION STATUS: written against the live Sui SDK; compiles under
//! `--features live`. Not built/run in the offline CI here. Field names match the
//! Cetus/Turbos pool structs used by the parity-proven Python readers
//! (`validation/cetus/cetus_rpc.py`, `turbos_rpc.py`).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Context, Result};

use crate::cache::ReserveCache;
use crate::clmm::ClmmState;
use crate::config::Config;
use crate::quoter::LivePoolRef;
use crate::types::{Dex, PoolState};

/// Thread-safe map of pool id → on-chain coordinates, for the quoter + PTB builder.
pub type LiveRegistry = Arc<RwLock<HashMap<String, LivePoolRef>>>;

/// A pool to track, parsed from `config.tracked_pools` (`"<dex>:<object_id>"`).
struct Tracked {
    dex: Dex,
    pool_id: String,
}

fn parse_tracked(entries: &[String]) -> Result<Vec<Tracked>> {
    entries
        .iter()
        .map(|e| {
            let (dex, id) = e
                .split_once(':')
                .context("tracked pool must be '<dex>:<id>'")?;
            let dex = match dex.trim().to_ascii_lowercase().as_str() {
                "cetus" => Dex::Cetus,
                "turbos" => Dex::Turbos,
                "amm_v2" | "ammv2" => Dex::AmmV2,
                other => return Err(anyhow!("unknown dex '{other}'")),
            };
            Ok(Tracked {
                dex,
                pool_id: id.trim().to_string(),
            })
        })
        .collect()
}

/// Hydrate the cache + registry with the current state of every tracked pool.
pub async fn bootstrap_pools(
    config: &Config,
    cache: &Arc<ReserveCache>,
    registry: &LiveRegistry,
) -> Result<()> {
    use sui_json_rpc_types::SuiObjectDataOptions;
    use sui_sdk::SuiClientBuilder;
    use sui_types::base_types::ObjectID;

    let client = SuiClientBuilder::default().build(&config.rpc_url).await?;
    let tracked = parse_tracked(&config.tracked_pools)?;
    if tracked.is_empty() {
        tracing::warn!("no tracked pools configured (ARB_TRACKED_POOLS)");
        return Ok(());
    }

    let ids: Vec<ObjectID> = tracked
        .iter()
        .map(|t| t.pool_id.parse())
        .collect::<Result<_, _>>()?;
    let opts = SuiObjectDataOptions::new()
        .with_content()
        .with_type()
        .with_owner();
    let objs = client
        .read_api()
        .multi_get_object_with_options(ids, opts)
        .await?;

    let mut n = 0;
    for (t, obj) in tracked.iter().zip(objs.into_iter()) {
        match decode_pool(t.dex, &obj) {
            Ok((state, lref)) => {
                cache.upsert(state);
                registry
                    .write()
                    .expect("registry poisoned")
                    .insert(t.pool_id.clone(), lref);
                n += 1;
            }
            Err(e) => tracing::warn!(pool = %t.pool_id, "decode failed: {e}"),
        }
    }
    tracing::info!(live_pools = n, "bootstrap complete");
    Ok(())
}

/// Subscribe to venue swap/liquidity events and refresh affected pools by re-reading
/// their objects (CLMMs have no reserve-delta events).
pub async fn run(
    config: &Config,
    cache: &Arc<ReserveCache>,
    registry: &LiveRegistry,
) -> Result<()> {
    use futures_util::StreamExt;
    use sui_json_rpc_types::{EventFilter, SuiObjectDataOptions};
    use sui_sdk::SuiClientBuilder;
    use sui_types::base_types::ObjectID;

    let client = SuiClientBuilder::default()
        .ws_url(&config.ws_url)
        .build(&config.rpc_url)
        .await?;

    // Track the set of pool ids we care about.
    let tracked: std::collections::HashSet<String> = parse_tracked(&config.tracked_pools)?
        .into_iter()
        .map(|t| t.pool_id)
        .collect();

    // Subscribe to swap events on each venue's CLMM module.
    let filters = vec![
        EventFilter::MoveModule {
            package: super::quoter::CETUS_PKG.parse()?,
            module: "pool".parse()?,
        },
        EventFilter::MoveModule {
            package: super::quoter::TURBOS_PKG.parse()?,
            module: "pool".parse()?,
        },
    ];
    let mut stream = client
        .event_api()
        .subscribe_event(EventFilter::Any(filters))
        .await?;
    let opts = SuiObjectDataOptions::new()
        .with_content()
        .with_type()
        .with_owner();
    tracing::info!("ws: subscribed to Cetus + Turbos pool events");

    while let Some(event) = stream.next().await {
        let event = event?;
        let Some(pool_id) = pool_id_from_event(&event) else {
            continue;
        };
        if !tracked.contains(&pool_id) {
            continue;
        }
        // The venue is known from the registry (we only got here because it's tracked).
        let dex = match registry.read().expect("registry poisoned").get(&pool_id) {
            Some(r) => r.dex,
            None => continue,
        };
        let Ok(id) = pool_id.parse::<ObjectID>() else {
            continue;
        };
        // Event-triggered re-read: pull the fresh pool object and upsert.
        match client
            .read_api()
            .get_object_with_options(id, opts.clone())
            .await
        {
            Ok(resp) => match decode_pool(dex, &resp) {
                Ok((state, lref)) => {
                    cache.upsert(state);
                    registry
                        .write()
                        .expect("registry poisoned")
                        .insert(pool_id.clone(), lref);
                }
                Err(e) => tracing::debug!(pool = %pool_id, "decode failed: {e}"),
            },
            Err(e) => tracing::debug!(pool = %pool_id, "re-read failed: {e}"),
        }
    }
    Ok(())
}

/// Extract the pool object id from a venue swap/liquidity event's parsed JSON.
fn pool_id_from_event(event: &sui_json_rpc_types::SuiEvent) -> Option<String> {
    event
        .parsed_json
        .get("pool")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Decode a venue pool object into `(PoolState, LivePoolRef)`.
fn decode_pool(
    dex: Dex,
    obj: &sui_json_rpc_types::SuiObjectResponse,
) -> Result<(PoolState, LivePoolRef)> {
    use sui_json_rpc_types::SuiParsedData;
    use sui_types::base_types::{ObjectID, SequenceNumber};

    let data = obj.data.as_ref().ok_or_else(|| anyhow!("no object data"))?;
    let type_str = data
        .type_
        .as_ref()
        .ok_or_else(|| anyhow!("no type"))?
        .to_string();
    let (type_a, type_b, fee_type) = parse_pool_type_args(&type_str)?;

    let Some(SuiParsedData::MoveObject(mv)) = &data.content else {
        return Err(anyhow!("no move content"));
    };
    let fields = mv.fields.to_json_value();

    let sqrt_price: u128 = json_u128(&fields, "current_sqrt_price")?;
    let liquidity: u128 = json_u128(&fields, "liquidity")?;
    let fee_rate: u64 = json_u64(&fields, "fee_rate").unwrap_or(0);

    let init_shared_version = match data.owner.as_ref() {
        Some(sui_types::object::Owner::Shared {
            initial_shared_version,
        }) => *initial_shared_version,
        _ => SequenceNumber::from_u64(0),
    };
    let pool_id: ObjectID = data.object_id;

    // Stage-1 estimate: single active range at current sqrt_price/liquidity. The
    // authoritative quoter reads full tick state before sizing (funnel stage 2).
    let state = PoolState::clmm(
        pool_id.to_string(),
        dex,
        type_a.to_string(),
        type_b.to_string(),
        ClmmState {
            sqrt_price,
            liquidity,
            fee_pips: fee_rate,
            ticks: Vec::new(),
        },
    );
    let lref = LivePoolRef {
        dex,
        pool_id,
        init_shared_version,
        type_a: type_a.parse()?,
        type_b: type_b.parse()?,
        fee_type: match fee_type {
            Some(ft) => Some(ft.parse()?),
            None => None,
        },
    };
    Ok((state, lref))
}

/// Split `…::pool::Pool<A, B[, FeeType]>` into its type arguments (depth-aware).
fn parse_pool_type_args(type_str: &str) -> Result<(String, String, Option<String>)> {
    let inside = type_str
        .split_once('<')
        .and_then(|(_, rest)| rest.rsplit_once('>').map(|(inner, _)| inner))
        .ok_or_else(|| anyhow!("type has no generic args: {type_str}"))?;
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in inside.chars() {
        match ch {
            '<' => {
                depth += 1;
                cur.push(ch);
            }
            '>' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    parts.push(cur.trim().to_string());
    let a = parts
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("missing type_a"))?;
    let b = parts
        .get(1)
        .cloned()
        .ok_or_else(|| anyhow!("missing type_b"))?;
    let fee = parts.get(2).cloned();
    Ok((a, b, fee))
}

fn json_u128(fields: &serde_json::Value, key: &str) -> Result<u128> {
    let v = fields
        .get(key)
        .ok_or_else(|| anyhow!("missing field {key}"))?;
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64().map(u128::from))
        .ok_or_else(|| anyhow!("field {key} not a u128"))
}

fn json_u64(fields: &serde_json::Value, key: &str) -> Result<u64> {
    let v = fields
        .get(key)
        .ok_or_else(|| anyhow!("missing field {key}"))?;
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .ok_or_else(|| anyhow!("field {key} not a u64"))
}
