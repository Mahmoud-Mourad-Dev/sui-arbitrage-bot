//! Obligation index (feature = "live"). Scallop obligations are shared objects created by
//! `open_obligation`. We:
//!   * bootstrap: `query_events` for the obligation-created event → seed ids, then read
//!     each object to fill collateral/debt positions;
//!   * maintain (`run`): over plain JSON-RPC (no `suix_subscribeEvent` — see `run`), a
//!     periodic loop that discovers newly-opened obligations and reconciles known ones by
//!     re-reading them. The single re-read seam (`refresh_obligation`) is where a local
//!     fullnode event stream plugs in for ~ms latency.
//!
//! VERIFICATION STATUS: the obligation decode (`decode_obligation` and helpers) is
//! **verified against mainnet** — layout from object `0xa042dcfd…` / tx `AYdhgWMq…`, with
//! pure parsers unit-tested and an end-to-end `--ignored` test
//! (`decode_real_mainnet_obligation`). Full ground truth: docs/scallop-liquidation-verified.md.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;
use sui_sdk::SuiClient;
use sui_types::base_types::ObjectID;
use sui_types::dynamic_field::DynamicFieldName;

use crate::config::Config;
use crate::liquidation::types::{Obligation, Position};
use crate::liquidation::ObligationIndex;
use crate::scanner::Protocol;

/// Scallop `protocol` package id (matches `ScallopProvider`/liquidate adapter).
pub const SCALLOP_PKG: &str = "0xefe8b36d5b2e43728cc323298626b83177803521d195cfb11e15b910e892fddf";

/// Bootstrap the index from historical obligation-created events.
pub async fn bootstrap(config: &Config, index: &ObligationIndex) -> Result<()> {
    use sui_json_rpc_types::EventFilter;
    use sui_sdk::SuiClientBuilder;

    let client = SuiClientBuilder::default().build(&config.rpc_url).await?;
    let filter = EventFilter::MoveModule {
        package: SCALLOP_PKG.parse()?,
        module: "open_obligation".parse()?,
    };

    let mut cursor = None;
    let mut seeded = 0usize;
    loop {
        let page = client
            .event_api()
            .query_events(filter.clone(), cursor, Some(200), false)
            .await?;
        for ev in &page.data {
            if let Some(id) = ev.parsed_json.get("obligation").and_then(|v| v.as_str()) {
                if let Err(e) = refresh_obligation(&client, id, index).await {
                    tracing::debug!(obligation = id, "bootstrap decode skipped: {e}");
                } else {
                    seeded += 1;
                }
            }
        }
        if !page.has_next_page {
            break;
        }
        cursor = page.next_cursor;
    }
    tracing::info!(obligations = seeded, "obligation index bootstrapped");
    Ok(())
}

/// Maintain the index over plain JSON-RPC — **no event subscription** (most providers,
/// incl. QuickNode, don't serve `suix_subscribeEvent`; the same reason pool ingestion
/// polls). Each cycle: (a) discover newly-opened obligations via `query_events`, then
/// (b) reconcile known obligations by re-reading them. For ~ms event latency, run a local
/// fullnode and switch this to its event stream (the seam is `refresh_obligation`).
pub async fn run(config: &Config, index: &ObligationIndex) -> Result<()> {
    use sui_json_rpc_types::EventFilter;
    use sui_sdk::SuiClientBuilder;

    let client = SuiClientBuilder::default().build(&config.rpc_url).await?;
    let filter = EventFilter::MoveModule {
        package: SCALLOP_PKG.parse()?,
        module: "open_obligation".parse()?,
    };
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tracing::info!("obligation index: RPC reconcile loop (no subscribe)");

    loop {
        tick.tick().await;

        // (a) Discover recently-opened obligations not yet indexed.
        match crate::metrics::time_rpc(
            crate::metrics::Rpc::QueryEvents,
            client
                .event_api()
                .query_events(filter.clone(), None, Some(50), true),
        )
        .await
        {
            Ok(page) => {
                for ev in &page.data {
                    if let Some(id) = ev.parsed_json.get("obligation").and_then(|v| v.as_str()) {
                        let known = index.read().expect("index poisoned").contains_key(id);
                        if !known {
                            if let Err(e) = refresh_obligation(&client, id, index).await {
                                tracing::debug!(obligation = id, "discover decode skipped: {e}");
                            }
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("obligation discover query failed: {e}"),
        }

        // (b) Reconcile known obligations (repairs missed mutations).
        let ids: Vec<String> = index
            .read()
            .expect("index poisoned")
            .keys()
            .cloned()
            .collect();
        crate::metrics::set_indexed_obligations(ids.len());
        for id in ids {
            if let Err(e) = refresh_obligation(&client, &id, index).await {
                tracing::debug!(obligation = %id, "reconcile skipped: {e}");
            }
        }
    }
}

/// Re-read one obligation and upsert its decoded positions. This is the single seam a
/// local-fullnode event stream would call instead of the reconcile loop.
async fn refresh_obligation(client: &SuiClient, id: &str, index: &ObligationIndex) -> Result<()> {
    let ob = decode_obligation(client, id).await?;
    index
        .write()
        .expect("index poisoned")
        .insert(id.to_string(), ob);
    Ok(())
}

/// Decode a Scallop obligation into protocol-agnostic positions by walking the dynamic
/// fields of its collateral/debt tables.
///
/// VERIFIED against mainnet (obligation `0xa042dcfd…`, tx `AYdhgWMq…`): `collaterals`/`debts`
/// are `WitTable`s whose entries hang off the **inner `table`'s** UID; each entry value is
/// `Collateral{amount}` / `Debt{amount,…}`. See `extract_table_id` and
/// docs/scallop-liquidation-verified.md §2. Defensive by design: an unrecognized layout
/// yields **empty** positions ⇒ the obligation reads as "no debt/collateral" ⇒ the source
/// never acts on it (cannot misfire capital).
async fn decode_obligation(client: &SuiClient, id: &str) -> Result<Obligation> {
    use sui_json_rpc_types::{SuiObjectDataOptions, SuiParsedData};

    let obj = client
        .read_api()
        .get_object_with_options(
            id.parse()?,
            SuiObjectDataOptions::new().with_content().with_type(),
        )
        .await?;
    let data = obj.data.ok_or_else(|| anyhow!("no object data"))?;
    let Some(SuiParsedData::MoveObject(mv)) = &data.content else {
        return Err(anyhow!("obligation has no move content"));
    };
    let content = mv.fields.clone().to_json_value();

    let mut collaterals = Vec::new();
    if let Some(tid) = extract_table_id(&content, "collaterals") {
        collaterals = read_positions(client, tid).await.unwrap_or_default();
    }
    let mut debts = Vec::new();
    if let Some(tid) = extract_table_id(&content, "debts") {
        debts = read_positions(client, tid).await.unwrap_or_default();
    }
    Ok(Obligation {
        id: id.to_string(),
        protocol: Protocol::Scallop,
        collaterals,
        debts,
    })
}

/// Enumerate a table's dynamic fields into positions: coin type ← field name (a coin
/// `TypeName`), amount ← the field value's `amount`.
async fn read_positions(client: &SuiClient, table_id: ObjectID) -> Result<Vec<Position>> {
    use sui_json_rpc_types::SuiParsedData;

    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = client
            .read_api()
            .get_dynamic_fields(table_id, cursor, Some(100))
            .await?;
        for info in &page.data {
            let Some(coin_type) = coin_type_from_name(&info.name) else {
                continue;
            };
            let field = client
                .read_api()
                .get_dynamic_field_object(table_id, info.name.clone())
                .await?;
            let amount = field
                .data
                .as_ref()
                .and_then(|d| d.content.as_ref())
                .and_then(|c| match c {
                    SuiParsedData::MoveObject(mv) => Some(mv.fields.clone().to_json_value()),
                    SuiParsedData::Package(_) => None,
                })
                .and_then(|v| amount_from_value(&v));
            if let Some(amount) = amount {
                if amount > 0 {
                    out.push(Position { coin_type, amount });
                }
            }
        }
        if !page.has_next_page {
            break;
        }
        cursor = page.next_cursor;
    }
    Ok(out)
}

// ── Pure decode helpers (offline-tested) ─────────────────────────────────────────────

/// Find the entry-holding `UID` for obligation field `field`.
///
/// VERIFIED (mainnet obligation `0xa042dcfd…`, tx `AYdhgWMq…`): the obligation stores
/// `collaterals`/`debts` as `wit_table::WitTable<…, TypeName, Collateral|Debt>`, whose
/// shape is `{ id: UID, table: 0x2::table::Table<…>, keys, with_keys }`. The dynamic-field
/// entries hang off the **inner `table`'s** UID — the WitTable's own `id` has **zero**
/// dynamic fields. So we descend into `table` first, then take its UID. (The previous
/// "first UID in the subtree" grabbed the empty WitTable UID — fixed.)
fn extract_table_id(content: &Value, field: &str) -> Option<ObjectID> {
    let node = content.get(field)?;
    let table = find_key(node, "table")?;
    find_uid(table)
}

/// First descendant object holding key `key` (handles the `{type, fields}` RPC wrapper
/// and the flattened `to_json_value` shape alike).
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if let Value::Object(map) = v {
        if let Some(found) = map.get(key) {
            return Some(found);
        }
        for val in map.values() {
            if let Some(found) = find_key(val, key) {
                return Some(found);
            }
        }
    }
    None
}

fn find_uid(v: &Value) -> Option<ObjectID> {
    if let Value::Object(map) = v {
        if let Some(s) = map.get("id").and_then(extract_id_str) {
            if let Ok(oid) = s.parse() {
                return Some(oid);
            }
        }
        for val in map.values() {
            if let Some(oid) = find_uid(val) {
                return Some(oid);
            }
        }
    }
    None
}

/// `id` may be a bare `"0x.."` string or a nested `UID { id: "0x.." }` object.
fn extract_id_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.starts_with("0x") => Some(s.clone()),
        Value::Object(m) => m.get("id").and_then(extract_id_str),
        _ => None,
    }
}

/// Render a dynamic-field name (a coin `TypeName`) to a canonical `0x..::module::Type`.
fn coin_type_from_name(name: &DynamicFieldName) -> Option<String> {
    type_name_from_json(&name.value)
}

fn type_name_from_json(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(normalize_type(s)),
        // TypeName is commonly `{ "name": "<addr>::mod::T" }`.
        Value::Object(m) => m.get("name").and_then(type_name_from_json),
        _ => None,
    }
}

fn normalize_type(s: &str) -> String {
    let s = s.trim();
    if s.starts_with("0x") {
        s.to_string()
    } else {
        format!("0x{s}")
    }
}

/// Pull the `amount` (u64) out of a table entry's value JSON, tolerating the dynamic-field
/// `Field { name, value }` and `{ fields: { .. } }` wrappers Sui adds.
fn amount_from_value(v: &Value) -> Option<u64> {
    if let Some(a) = v.get("amount").and_then(json_u64) {
        return Some(a);
    }
    for key in ["value", "fields"] {
        if let Some(a) = v.get(key).and_then(amount_from_value) {
            return Some(a);
        }
    }
    None
}

fn json_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn amount_from_value_handles_wrappers() {
        assert_eq!(
            amount_from_value(&json!({ "amount": "12345" })),
            Some(12345)
        );
        assert_eq!(amount_from_value(&json!({ "amount": 678 })), Some(678));
        // VERIFIED real Collateral entry shape (tx AYdhgWMq…, obligation 0xa042dcfd…):
        // get_dynamic_field_object → content.fields = { id, name, value: { type, fields: { amount } } }
        let entry = json!({
            "id": { "id": "0x0d224e…" },
            "name": { "type": "0x1::type_name::TypeName", "fields": { "name": "bde4…::hasui::HASUI" } },
            "value": { "type": "…::obligation_collaterals::Collateral", "fields": { "amount": "16780493800927" } }
        });
        assert_eq!(amount_from_value(&entry), Some(16_780_493_800_927));
        // Debt entry carries amount + borrow_index; we take amount.
        let debt = json!({ "value": { "fields": { "amount": "5000000000", "borrow_index": "1000000001" } } });
        assert_eq!(amount_from_value(&debt), Some(5_000_000_000));
        assert_eq!(amount_from_value(&json!({ "nope": 1 })), None);
    }

    #[test]
    fn extract_table_id_descends_into_table_not_wittable() {
        // VERIFIED shape: WitTable { id: <OWN uid, 0 entries>, keys, table: Table { id: <ENTRY uid> }, with_keys }.
        // Must return the inner table UID (…07), NOT the outer WitTable UID (…0a).
        let content = json!({
            "collaterals": { "fields": {
                "id": { "id": "0x000000000000000000000000000000000000000000000000000000000000000a" },
                "keys": { "fields": { "contents": [] } },
                "table": { "fields": { "id": { "id": "0x0000000000000000000000000000000000000000000000000000000000000007" }, "size": "2" } },
                "with_keys": true
            } },
            "debts": { "fields": {
                "id": { "id": "0x000000000000000000000000000000000000000000000000000000000000000b" },
                "table": { "fields": { "id": { "id": "0x0000000000000000000000000000000000000000000000000000000000000009" } } }
            } }
        });
        assert_eq!(
            extract_table_id(&content, "collaterals")
                .unwrap()
                .to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000007"
        );
        assert_eq!(
            extract_table_id(&content, "debts").unwrap().to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000009"
        );
        assert!(extract_table_id(&content, "missing").is_none());
    }

    /// End-to-end proof the dynamic-field walk decodes a REAL mainnet obligation. Ignored
    /// in CI (hits the public fullnode); run with:
    ///   `cargo test --features live -- --ignored decode_real_mainnet_obligation --nocapture`
    #[tokio::test]
    #[ignore = "hits mainnet RPC"]
    async fn decode_real_mainnet_obligation() {
        let client = sui_sdk::SuiClientBuilder::default()
            .build("https://fullnode.mainnet.sui.io:443")
            .await
            .unwrap();
        let ob = decode_obligation(
            &client,
            "0xa042dcfdb81ffc562537baee6b9820fb515ce0a207a71b9b121639fcb9661577",
        )
        .await
        .unwrap();
        // Must decode at least one position, each a well-formed coin type with amount > 0.
        assert!(
            !ob.collaterals.is_empty() || !ob.debts.is_empty(),
            "decoded zero positions — table walk broken"
        );
        for p in ob.collaterals.iter().chain(ob.debts.iter()) {
            assert!(p.coin_type.contains("::"), "bad coin type {}", p.coin_type);
            assert!(p.amount > 0, "zero amount for {}", p.coin_type);
        }
        println!("collaterals={:?}\ndebts={:?}", ob.collaterals, ob.debts);
    }

    #[test]
    fn type_name_normalizes_and_unwraps() {
        assert_eq!(
            type_name_from_json(&json!(
                "0000000000000000000000000000000000000000000000000000000000000002::sui::SUI"
            ))
            .unwrap(),
            "0x0000000000000000000000000000000000000000000000000000000000000002::sui::SUI"
        );
        assert_eq!(
            type_name_from_json(&json!({ "name": "0x2::usdc::USDC" })).unwrap(),
            "0x2::usdc::USDC"
        );
        assert!(type_name_from_json(&json!(42)).is_none());
    }
}
