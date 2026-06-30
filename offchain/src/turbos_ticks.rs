//! Turbos CLMM tick ingestion (feature = "live").
//!
//! Makes Turbos quotes depth-aware exactly like Cetus: load the pool's real initialized ticks
//! into the engine's [`TickBoundary`]s so [`crate::clmm::quote_exact_in`] traverses real
//! liquidity (and refuses sizes beyond it) instead of treating the pool as one infinite range.
//!
//! On-chain layout (VERIFIED mainnet, Turbos pkg `0x91bfbc38…`):
//! ```text
//! Pool                       : has `tick_current_index: I32`, `sqrt_price: u128`, `tick_map`.
//! Pool dynamic field per tick: 0x2::dynamic_field::Field<i32::I32, pool::Tick>
//!   Tick.liquidity_net       : i128::I128 { bits: u128 two's-complement }
//! ```
//! Unlike Cetus, a Turbos `Tick` does NOT store its `sqrt_price` — only the tick index (the
//! dynamic-field key). We derive each tick's `sqrt_price` from the pool's exact on-chain
//! `current_sqrt_price` and `tick_current_index`:
//!   `sqrt_price(tick) = current_sqrt_price · 1.0001^((tick − current_tick) / 2)`
//! Anchoring to the real current price means the (windowed) ticks near the price are accurate
//! to f64 precision — enough for Stage-1 depth-aware sizing (the on-chain dry-run is still the
//! authoritative gate).

use anyhow::Result;
use serde_json::Value;
use sui_json_rpc_types::{SuiObjectDataOptions, SuiObjectResponse, SuiParsedData};
use sui_sdk::SuiClient;
use sui_types::base_types::ObjectID;

use crate::clmm::TickBoundary;
use crate::metrics::{self, Rpc};

/// Tick index → skip-list-equivalent: Turbos uses the same 1.0001 tick base as Cetus.
const TICK_BASE_LN: f64 = 0.000_099_995_000_333_310_53; // ln(1.0001)

/// Parse the pool's `tick_current_index` (I32) from its content. `None` if absent/malformed.
#[must_use]
pub fn current_tick(obj: &SuiObjectResponse) -> Option<i32> {
    let data = obj.data.as_ref()?;
    let SuiParsedData::MoveObject(mv) = data.content.as_ref()? else {
        return None;
    };
    let content = mv.fields.clone().to_json_value();
    let cti = find_key(&content, "tick_current_index")?;
    let bits = json_u128(cti).or_else(|| find_key(cti, "bits").and_then(json_u128))? as u32;
    Some(bits as i32) // two's-complement reinterpret
}

/// Load up to `window` initialized ticks NEAREST the current tick, sorted ascending by the
/// derived `sqrt_price`. Tick objects are dynamic fields on the POOL itself (keyed by I32);
/// we enumerate their indices cheaply (names only), window by index, then fetch + decode only
/// the windowed nodes. Mirrors the Cetus windowed loader.
pub async fn load_window(
    client: &SuiClient,
    pool: ObjectID,
    current_sqrt_price: u128,
    current_tick: i32,
    window: usize,
) -> Result<Vec<TickBoundary>> {
    // 1. Enumerate (tick_index, node_id) for the pool's Tick dynamic fields (names only).
    let mut nodes: Vec<(i32, ObjectID)> = Vec::new();
    let mut cursor = None;
    loop {
        let page = metrics::time_rpc(
            Rpc::DynamicFields,
            client.read_api().get_dynamic_fields(pool, cursor, Some(50)),
        )
        .await?;
        for info in &page.data {
            // Keep only `pool::Tick` fields (the pool also holds `Position` fields).
            if !info.object_type.ends_with("::pool::Tick") {
                continue;
            }
            if let Some(idx) = json_u128(&info.name.value)
                .or_else(|| find_key(&info.name.value, "bits").and_then(json_u128))
            {
                nodes.push((idx as u32 as i32, info.object_id));
            }
        }
        if !page.has_next_page {
            break;
        }
        cursor = page.next_cursor;
    }
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    nodes.sort_unstable_by_key(|(t, _)| *t);
    let windowed = select_window(&nodes, current_tick, window);

    // 2. Fetch + decode only the windowed Tick objects; derive each sqrt_price from its index.
    let opts = SuiObjectDataOptions::new().with_content();
    let ids: Vec<ObjectID> = windowed.iter().map(|(_, id)| *id).collect();
    let mut out: Vec<TickBoundary> = Vec::with_capacity(ids.len());
    for chunk in windowed.chunks(50) {
        let chunk_ids: Vec<ObjectID> = chunk.iter().map(|(_, id)| *id).collect();
        let resp = metrics::time_rpc(
            Rpc::MultiGetObject,
            client
                .read_api()
                .multi_get_object_with_options(chunk_ids, opts.clone()),
        )
        .await?;
        for ((tick_idx, _), r) in chunk.iter().zip(resp.iter()) {
            if let Some(net) = decode_liquidity_net(r) {
                let sqrt_price = sqrt_price_at_tick(current_sqrt_price, current_tick, *tick_idx);
                out.push(TickBoundary {
                    sqrt_price,
                    liquidity_net: net,
                });
            }
        }
    }
    let _ = ids; // (ids kept for symmetry/debugging)
    out.sort_by_key(|t| t.sqrt_price);
    Ok(out)
}

/// `sqrt_price(tick) = current_sqrt_price · 1.0001^((tick − current_tick) / 2)`, anchored to
/// the exact on-chain current price for accuracy near the price (windowed ticks).
fn sqrt_price_at_tick(current_sqrt_price: u128, current_tick: i32, tick: i32) -> u128 {
    if tick == current_tick {
        return current_sqrt_price; // exact at the current tick (no f64 rounding)
    }
    let delta = f64::from(tick - current_tick);
    let mult = (delta * 0.5 * TICK_BASE_LN).exp();
    let v = current_sqrt_price as f64 * mult;
    if v.is_finite() && v >= 0.0 {
        v as u128
    } else {
        current_sqrt_price
    }
}

/// Pick up to `window` node ids whose tick index is nearest `center`, from an index-sorted slice.
fn select_window(sorted: &[(i32, ObjectID)], center: i32, window: usize) -> Vec<(i32, ObjectID)> {
    if sorted.len() <= window {
        return sorted.to_vec();
    }
    let pos = sorted.partition_point(|(t, _)| *t < center);
    let start = pos.saturating_sub(window / 2);
    let end = (start + window).min(sorted.len());
    let start = end.saturating_sub(window);
    sorted[start..end].to_vec()
}

/// Decode `Tick.liquidity_net` (I128 two's-complement) from a fetched node's content.
fn decode_liquidity_net(resp: &SuiObjectResponse) -> Option<i128> {
    let data = resp.data.as_ref()?;
    let SuiParsedData::MoveObject(mv) = data.content.as_ref()? else {
        return None;
    };
    let node = mv.fields.clone().to_json_value();
    let net = find_key(&node, "liquidity_net")?;
    let bits = json_u128(net).or_else(|| find_key(net, "bits").and_then(json_u128))?;
    Some(bits as i128)
}

// ── JSON helpers (local; mirror cetus_ticks) ──────────────────────────────────────────────

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if let Value::Object(m) = v {
        if let Some(x) = m.get(key) {
            return Some(x);
        }
        for val in m.values() {
            if let Some(x) = find_key(val, key) {
                return Some(x);
            }
        }
    }
    None
}

fn json_u128(v: &Value) -> Option<u128> {
    v.as_u64()
        .map(u128::from)
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decode_liquidity_net_from_turbos_tick_shape() {
        // VERIFIED node shape: value.fields.liquidity_net.fields.bits (I128 two's-complement).
        let node = json!({
            "id": { "id": "0xabc" },
            "name": { "fields": { "bits": 443600 } },
            "value": { "fields": {
                "liquidity_gross": "1981",
                "liquidity_net": { "fields": { "bits": "340282366920938463463374607431768209475" } },
                "initialized": true
            } }
        });
        // (decode_liquidity_net takes a SuiObjectResponse; here we test the inner JSON path.)
        let net = find_key(&node, "liquidity_net").unwrap();
        let bits = json_u128(net)
            .or_else(|| find_key(net, "bits").and_then(json_u128))
            .unwrap();
        assert_eq!(bits as i128, -1981); // 2^128 - 1981
    }

    #[test]
    fn sqrt_price_anchors_to_current() {
        let cur: u128 = 361_869_506_226_507_097;
        // At the current tick the multiplier is 1 → exactly the current price.
        assert_eq!(sqrt_price_at_tick(cur, -78632, -78632), cur);
        // One tick up is ~1.0001^0.5 higher; one tick down is lower; strictly monotonic.
        let up = sqrt_price_at_tick(cur, -78632, -78630);
        let down = sqrt_price_at_tick(cur, -78632, -78634);
        assert!(
            down < cur && cur < up,
            "sqrt_price must increase with tick index"
        );
    }

    #[test]
    fn select_window_centers_on_tick() {
        let nodes: Vec<_> = (-50..50i32).map(|t| (t * 2, ObjectID::random())).collect();
        let w = select_window(&nodes, 0, 10);
        assert_eq!(w.len(), 10);
        assert!(
            w.iter().all(|(t, _)| t.abs() <= 12),
            "window brackets the center"
        );
    }
}
