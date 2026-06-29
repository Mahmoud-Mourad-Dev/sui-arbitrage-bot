//! Cetus CLMM tick ingestion (feature = "live").
//!
//! Reads a Cetus pool's initialized ticks from its on-chain `SkipList` and decodes each into
//! the engine's [`TickBoundary`] `{ sqrt_price, liquidity_net }`, so Stage-1 quotes traverse
//! real liquidity instead of treating the whole pool as one infinite range (the empty-ticks
//! approximation that produced impossible over-quotes).
//!
//! On-chain layout (VERIFIED mainnet, pool `0xc8d7a150…`):
//! ```text
//! Pool.tick_manager.ticks : 0xbe21a06…::skip_list::SkipList<0x1eabed72…::tick::Tick>
//!   each node             : 0x2::dynamic_field::Field<u64, skip_list::Node<Tick>>
//!     node.value.value (Tick) : { index:I32, sqrt_price:u128, liquidity_net:I128{bits:u128}, … }
//! ```
//! `liquidity_net` is a Cetus `I128` whose `bits:u128` is the two's-complement value, so the
//! signed delta is `bits as i128`. The tick `index` is not needed — the engine keys on
//! `sqrt_price` (Q64.64, same scale as the pool's `current_sqrt_price`).
//!
//! NOTE: this loads (up to a cap) the pool's initialized ticks via dynamic fields. For thin
//! pools (the source of the impossible quotes) that is a handful of cheap reads; for very
//! liquid pools it is RPC-heavy, which is why it is gated behind `ARB_INDEXER_LOAD_TICKS`
//! (default off) and capped. A windowed/cached loader is the production follow-up.

use anyhow::Result;
use serde_json::Value;
use sui_json_rpc_types::{SuiObjectDataOptions, SuiObjectResponse, SuiParsedData};
use sui_sdk::SuiClient;
use sui_types::base_types::ObjectID;

use crate::clmm::TickBoundary;
use crate::metrics::{self, Rpc};

/// Parse the `SkipList` UID from a Cetus pool object's content (`tick_manager.ticks`).
#[must_use]
pub fn skiplist_id(obj: &SuiObjectResponse) -> Option<ObjectID> {
    let data = obj.data.as_ref()?;
    let SuiParsedData::MoveObject(mv) = data.content.as_ref()? else {
        return None;
    };
    let content = mv.fields.clone().to_json_value();
    let tm = find_key(&content, "tick_manager")?;
    let ticks = find_key(tm, "ticks")?;
    find_uid(ticks)
}

/// Load initialized ticks (sorted ascending by `sqrt_price`), capped at `cap`.
pub async fn load(client: &SuiClient, skiplist: ObjectID, cap: usize) -> Result<Vec<TickBoundary>> {
    // 1. Enumerate the skip-list node object ids (dynamic fields).
    let mut node_ids: Vec<ObjectID> = Vec::new();
    let mut cursor = None;
    loop {
        let page = metrics::time_rpc(
            Rpc::DynamicFields,
            client
                .read_api()
                .get_dynamic_fields(skiplist, cursor, Some(50)),
        )
        .await?;
        for info in &page.data {
            node_ids.push(info.object_id);
        }
        if !page.has_next_page || node_ids.len() >= cap {
            break;
        }
        cursor = page.next_cursor;
    }
    node_ids.truncate(cap);

    // 2. Batch-fetch node objects + decode each Tick.
    let opts = SuiObjectDataOptions::new().with_content();
    let mut out: Vec<TickBoundary> = Vec::with_capacity(node_ids.len());
    for chunk in node_ids.chunks(50) {
        let resp = metrics::time_rpc(
            Rpc::MultiGetObject,
            client
                .read_api()
                .multi_get_object_with_options(chunk.to_vec(), opts.clone()),
        )
        .await?;
        for r in &resp {
            if let Some(tb) = decode_node(r) {
                out.push(tb);
            }
        }
    }
    out.sort_by_key(|t| t.sqrt_price);
    Ok(out)
}

fn decode_node(resp: &SuiObjectResponse) -> Option<TickBoundary> {
    let data = resp.data.as_ref()?;
    let SuiParsedData::MoveObject(mv) = data.content.as_ref()? else {
        return None;
    };
    decode_tick(&mv.fields.clone().to_json_value())
}

/// Pure: extract `sqrt_price` + `liquidity_net` from a node's content JSON. Tolerant of the
/// `{type,fields}` RPC wrapper and the flattened `to_json_value` shape. Offline-tested.
fn decode_tick(node: &Value) -> Option<TickBoundary> {
    let sqrt_price = find_key(node, "sqrt_price").and_then(json_u128)?;
    let net = find_key(node, "liquidity_net")?;
    // I128 renders as `{bits: u128}` (or, if flattened, directly the value).
    let bits = json_u128(net).or_else(|| find_key(net, "bits").and_then(json_u128))?;
    Some(TickBoundary {
        sqrt_price,
        liquidity_net: bits as i128, // two's-complement reinterpret (Cetus I128)
    })
}

// ── recursive JSON helpers ────────────────────────────────────────────────────────────

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

fn find_uid(v: &Value) -> Option<ObjectID> {
    if let Value::Object(m) = v {
        if let Some(s) = m.get("id").and_then(extract_id_str) {
            if let Ok(oid) = s.parse() {
                return Some(oid);
            }
        }
        for val in m.values() {
            if let Some(oid) = find_uid(val) {
                return Some(oid);
            }
        }
    }
    None
}

fn extract_id_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.starts_with("0x") => Some(s.clone()),
        Value::Object(m) => m.get("id").and_then(extract_id_str),
        _ => None,
    }
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
    fn decode_tick_from_skiplist_node_shape() {
        // VERIFIED shape: Field { value: Node { …, value: Tick { sqrt_price, liquidity_net:{bits} } } }
        let node = json!({
            "id": { "id": "0xabc" },
            "name": "444118",
            "value": { "fields": { "score": "444118",
                "value": { "fields": {
                    "index": { "fields": { "bits": 482 } },
                    "sqrt_price": "18896688158914253353",
                    "liquidity_net": { "fields": { "bits": "340282366920938463463374607431768211455" } },
                    "liquidity_gross": "15127"
                } } } }
        });
        let tb = decode_tick(&node).unwrap();
        assert_eq!(tb.sqrt_price, 18896688158914253353);
        assert_eq!(tb.liquidity_net, -1); // 2^128-1 → -1 (two's complement)
    }

    #[test]
    fn decode_tick_positive_liquidity_net() {
        let node = json!({ "sqrt_price": "1000", "liquidity_net": { "bits": "5000" } });
        let tb = decode_tick(&node).unwrap();
        assert_eq!(tb.sqrt_price, 1000);
        assert_eq!(tb.liquidity_net, 5000);
    }

    /// End-to-end verification against a REAL mainnet Cetus pool. Ignored in CI (hits the
    /// public fullnode); run with:
    ///   `cargo test --features live -- --ignored verify_ticks_match_chain --nocapture`
    ///
    /// Proves, on real data: (a) every initialized tick is loaded (loaded == independently
    /// enumerated count ⇒ no skips, complete pagination), (b) no duplicate node ids, (c) ticks
    /// strictly ascending + unique sqrt_price, and (d) the strongest invariant — across ALL
    /// ticks `Σ liquidity_net == 0` (CLMM positions add at the lower tick and remove at the
    /// upper). A wrong sign (i128), a skipped/duplicated tick, or truncation would break (d).
    #[tokio::test]
    #[ignore = "hits mainnet RPC"]
    async fn verify_ticks_match_chain() {
        use std::collections::HashSet;
        use sui_sdk::SuiClientBuilder;
        const POOL: &str = "0xc8d7a1503dc2f9f5b05449a87d8733593e2f0f3e7bffd90541252782e4d2ca20";
        const CAP: usize = 5000;

        let client = SuiClientBuilder::default()
            .build("https://fullnode.mainnet.sui.io:443")
            .await
            .unwrap();
        let obj = client
            .read_api()
            .get_object_with_options(
                POOL.parse().unwrap(),
                SuiObjectDataOptions::new().with_content().with_type(),
            )
            .await
            .unwrap();
        let sk = skiplist_id(&obj).expect("skiplist id from pool content");

        // Independently enumerate every node id (detect dups + complete pagination).
        let mut ids = HashSet::new();
        let mut dups = 0usize;
        let mut cursor = None;
        loop {
            let page = client
                .read_api()
                .get_dynamic_fields(sk, cursor, Some(50))
                .await
                .unwrap();
            for info in &page.data {
                if !ids.insert(info.object_id) {
                    dups += 1;
                }
            }
            if !page.has_next_page || ids.len() >= CAP {
                break;
            }
            cursor = page.next_cursor;
        }
        assert_eq!(dups, 0, "duplicate skip-list node ids in pagination");
        let n = ids.len();

        let ticks = load(&client, sk, CAP).await.unwrap();
        assert_eq!(
            ticks.len(),
            n,
            "loaded {} != enumerated {n} (skip/decode failure)",
            ticks.len()
        );
        for w in ticks.windows(2) {
            assert!(
                w[0].sqrt_price < w[1].sqrt_price,
                "ticks not strictly ascending / duplicate sqrt_price"
            );
        }
        let pos = ticks.iter().filter(|t| t.liquidity_net > 0).count();
        let neg = ticks.iter().filter(|t| t.liquidity_net < 0).count();
        let sum: i128 = ticks.iter().map(|t| t.liquidity_net).sum();
        println!(
            "pool {POOL}: ticks={n}, +net={pos}, -net={neg}, Σliquidity_net={sum} (capped={})",
            n >= CAP
        );
        assert!(pos > 0 && neg > 0, "expected both signs of liquidity_net");
        if n < CAP {
            assert_eq!(sum, 0, "Σ liquidity_net across all ticks must be 0");
        }
    }

    #[test]
    fn decode_tick_missing_fields_is_none() {
        assert!(decode_tick(&json!({ "sqrt_price": "1" })).is_none()); // no liquidity_net
        assert!(decode_tick(&json!({ "nope": 1 })).is_none());
    }

    #[test]
    fn skiplist_uid_extracted_from_tick_manager() {
        // Mirror the pool content path tick_manager → ticks → id.
        let content = json!({
            "current_sqrt_price": "123",
            "tick_manager": { "fields": { "tick_spacing": 2, "ticks": { "fields": {
                "id": { "id": "0x471d30a44388756ae2aae81e2ed313e7cce31b767b688936abd6930eeecd93e2" },
                "head": [], "level": 1 } } } }
        });
        let tm = find_key(&content, "tick_manager").unwrap();
        let ticks = find_key(tm, "ticks").unwrap();
        assert_eq!(
            find_uid(ticks).unwrap().to_string(),
            "0x471d30a44388756ae2aae81e2ed313e7cce31b767b688936abd6930eeecd93e2"
        );
    }
}
