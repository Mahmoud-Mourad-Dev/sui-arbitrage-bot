//! Checkpoint-driven, diff-based pool ingestion (feature = "live").
//!
//! The original ingestion (`ws::run`) re-reads **every** tracked pool on a fixed wall-clock
//! interval, whether or not anything changed. This module instead drives off chain
//! progress:
//!
//!   1. Poll the **checkpoint tip** (`get_latest_checkpoint_sequence_number`) — a single
//!      tiny RPC. On a quiet chain (no new checkpoint) we do **zero** object reads.
//!   2. When the tip advances, do **one batched** `multi_get_object` and **diff by object
//!      version** — decode + upsert only the pools that actually changed.
//!
//! This (a) ties refresh cadence to real chain advancement, (b) eliminates redundant
//! decode/registry writes, and (c) is the drop-in seam for a co-located fullnode
//! checkpoint **stream** (gRPC `SubscribeCheckpoints`), which pushes the same per-checkpoint
//! object writes at ~ms latency — the real production win. Over a remote JSON-RPC the
//! latency floor is the tip-poll RTT; point `SUI_RPC_URL` at a local node to collapse it.
//!
//! It reuses the executor's shared `SuiClient` (no extra connection) and the `ws` decode
//! helpers (single source of truth for venue field parsing).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use sui_json_rpc_types::SuiObjectDataOptions;
use sui_sdk::SuiClient;
use sui_types::base_types::ObjectID;

use crate::cache::ReserveCache;
use crate::config::Config;
use crate::ws::{self, LiveRegistry};

/// Watch the checkpoint tip and refresh only the tracked pools whose object version
/// changed since the last advance. Runs until the client errors irrecoverably.
pub async fn run_checkpoint_diff(
    config: &Config,
    cache: &Arc<ReserveCache>,
    registry: &LiveRegistry,
    client: &SuiClient,
) -> Result<()> {
    let tracked = ws::parse_tracked(&config.tracked_pools)?;
    let ids: Vec<ObjectID> = tracked
        .iter()
        .map(|t| t.pool_id.parse())
        .collect::<Result<_, _>>()?;
    if ids.is_empty() {
        tracing::warn!("checkpoint ingest: no tracked pools (ARB_TRACKED_POOLS)");
        return Ok(());
    }
    let opts = SuiObjectDataOptions::new()
        .with_content()
        .with_type()
        .with_owner();

    // Tip poll is a tiny RPC; keep it responsive but bounded.
    let tip_ms = config.poll_interval_ms.clamp(100, 1_000);
    let mut tip = tokio::time::interval(Duration::from_millis(tip_ms));
    let mut seen: HashMap<String, u64> = HashMap::new();
    let mut last_cp: u64 = 0;
    crate::metrics::set_tracked_pools(ids.len());
    tracing::info!(
        pools = ids.len(),
        tip_ms,
        "pool refresh: checkpoint-diff (refresh only on chain advance + version change)"
    );

    loop {
        tip.tick().await;

        // 1. Cheap: has the chain advanced?
        let cp = match crate::metrics::time_rpc(
            crate::metrics::Rpc::LatestCheckpoint,
            client.read_api().get_latest_checkpoint_sequence_number(),
        )
        .await
        {
            Ok(c) => {
                crate::metrics::set_rpc_up(true);
                crate::metrics::set_latest_checkpoint(c);
                c
            }
            Err(e) => {
                crate::metrics::set_rpc_up(false);
                tracing::warn!("checkpoint tip rpc failed: {e}");
                continue;
            }
        };
        if cp <= last_cp {
            continue; // no new checkpoint → no work, no object reads
        }
        last_cp = cp;
        let _t = crate::metrics::stage_timer(crate::metrics::Stage::Ingestion);

        // 2. One batched read; diff by version.
        let objs = match crate::metrics::time_rpc(
            crate::metrics::Rpc::MultiGetObject,
            client
                .read_api()
                .multi_get_object_with_options(ids.clone(), opts.clone()),
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                crate::metrics::set_rpc_up(false);
                tracing::warn!("pool refresh rpc failed: {e}");
                continue;
            }
        };
        let fresh: Vec<(String, u64)> = tracked
            .iter()
            .zip(&objs)
            .filter_map(|(t, obj)| {
                obj.data
                    .as_ref()
                    .map(|d| (t.pool_id.clone(), d.version.value()))
            })
            .collect();
        let changed = changed_since(&mut seen, &fresh);
        if changed.is_empty() {
            continue;
        }

        // 3. Decode + upsert ONLY the changed pools.
        let mut n = 0u32;
        for (t, obj) in tracked.iter().zip(&objs) {
            if !changed.contains(&t.pool_id) {
                continue;
            }
            match ws::decode_pool(t.dex, obj) {
                Ok((state, lref)) => {
                    cache.upsert(state);
                    registry
                        .write()
                        .expect("registry poisoned")
                        .insert(t.pool_id.clone(), lref);
                    n += 1;
                }
                Err(e) => tracing::debug!(pool = %t.pool_id, "refresh decode failed: {e}"),
            }
        }
        crate::metrics::inc_pools_updated(u64::from(n));
        crate::metrics::set_cache_size(cache.len());
        tracing::debug!(checkpoint = cp, changed = n, "pools refreshed");
    }
}

/// Pure change-detection: return the pool ids whose version advanced vs `seen` (or are
/// new), updating `seen` in place. A read with a version `<=` the last seen (a stale/lagging
/// replica) is treated as unchanged. Pools absent from `fresh` are left untouched.
fn changed_since(seen: &mut HashMap<String, u64>, fresh: &[(String, u64)]) -> Vec<String> {
    let mut changed = Vec::new();
    for (id, ver) in fresh {
        match seen.get(id) {
            Some(prev) if prev >= ver => {} // unchanged or stale read → skip
            _ => {
                seen.insert(id.clone(), *ver);
                changed.push(id.clone());
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_since_detects_new_advanced_and_ignores_stale() {
        let mut seen: HashMap<String, u64> = HashMap::new();
        let f1: Vec<(String, u64)> = vec![("a".into(), 5), ("b".into(), 3)];

        // First sighting: everything is "changed".
        let mut c = changed_since(&mut seen, &f1);
        c.sort();
        assert_eq!(c, vec!["a".to_string(), "b".to_string()]);

        // Re-read identical versions → nothing changed.
        assert!(changed_since(&mut seen, &f1).is_empty());

        // `a` advances, `b` stays → only `a`.
        let f2: Vec<(String, u64)> = vec![("a".into(), 6), ("b".into(), 3)];
        assert_eq!(changed_since(&mut seen, &f2), vec!["a".to_string()]);

        // A stale/lagging read (lower version) is ignored.
        let f3: Vec<(String, u64)> = vec![("a".into(), 4)];
        assert!(changed_since(&mut seen, &f3).is_empty());

        // A brand-new pool appears mid-run.
        let f4: Vec<(String, u64)> = vec![("c".into(), 1)];
        assert_eq!(changed_since(&mut seen, &f4), vec!["c".to_string()]);
    }
}
