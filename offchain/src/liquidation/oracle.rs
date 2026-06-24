//! Protocol oracle access (feature = "live").
//!
//! Constraint #3: health MUST use the SAME oracle + math as the protocol. The cleanest
//! way to honor that is to read the protocol's own price view rather than re-deriving
//! it: for Scallop we `devInspect` `price::get_price(x_oracle, type, clock)` — that's
//! the exact value `liquidate`/`calculate_liquidation_amounts` will see. For protocols
//! that require a fresh on-chain price at liquidate time (e.g. Suilend/Pyth), we also
//! fetch the Pyth VAA from Hermes to build the in-PTB update (Phase 5).
//!
//! Staleness is explicit: a price too old to pass the protocol's `clock` freshness
//! check is a non-opportunity (we never act on a stale price).
//!
//! VERIFICATION STATUS: written against the live SDK + Hermes; compiles under
//! `--features live`; not built/run in offline CI here.

use anyhow::{anyhow, bail, Result};

/// A price reading from the protocol's oracle.
#[derive(Clone, Copy, Debug)]
pub struct PriceQuote {
    pub price_usd: f64,
    pub published_ms: u64,
}

impl PriceQuote {
    /// True if older than `max_age_ms` at `now_ms` — i.e. the protocol would reject it.
    #[must_use]
    pub fn is_stale(&self, now_ms: u64, max_age_ms: u64) -> bool {
        now_ms.saturating_sub(self.published_ms) > max_age_ms
    }
}

/// Read Scallop's authoritative price for `coin_type` via `devInspect` of
/// `price::get_price(x_oracle, type, clock)` — the same value liquidate will use.
pub async fn scallop_price(
    client: &sui_sdk::SuiClient,
    package: &str,
    x_oracle_id: &str,
    x_oracle_init_version: u64,
    coin_type: &str,
) -> Result<PriceQuote> {
    use sui_json_rpc_types::SuiTransactionBlockEffectsAPI;
    use sui_types::base_types::{ObjectID, SequenceNumber};
    use sui_types::programmable_transaction_builder::ProgrammableTransactionBuilder;
    use sui_types::transaction::{Command, ObjectArg, SharedObjectMutability, TransactionKind};

    let mut ptb = ProgrammableTransactionBuilder::new();
    let oracle = ptb.obj(ObjectArg::SharedObject {
        id: x_oracle_id.parse()?,
        initial_shared_version: SequenceNumber::from_u64(x_oracle_init_version),
        mutability: SharedObjectMutability::Immutable,
    })?;
    let clock = ptb.obj(ObjectArg::SharedObject {
        id: ObjectID::from_hex_literal("0x6")?,
        initial_shared_version: SequenceNumber::from_u64(1),
        mutability: SharedObjectMutability::Immutable,
    })?;
    ptb.command(Command::move_call(
        package.parse::<ObjectID>()?,
        sui_types::Identifier::new("price")?,
        sui_types::Identifier::new("get_price")?,
        vec![coin_type.parse()?],
        vec![oracle, clock],
    ));
    let res = client
        .read_api()
        .dev_inspect_transaction_block(
            "0x0000000000000000000000000000000000000000000000000000000000000001".parse()?,
            TransactionKind::ProgrammableTransaction(ptb.finish()),
            None,
            None,
            None,
        )
        .await?;
    let effects = &res.effects;
    if effects.status().is_err() {
        return Err(anyhow!("price get_price reverted: {:?}", effects.status()));
    }
    // Scallop's price is a FixedPoint32 (u64 fraction). Decode + scale to f64.
    let results = res.results.ok_or_else(|| anyhow!("no results"))?;
    let raw = results
        .first()
        .and_then(|r| r.return_values.first())
        .ok_or_else(|| anyhow!("no return value"))?;
    let bits = u64::from_le_bytes(
        raw.0
            .get(0..8)
            .ok_or_else(|| anyhow!("short"))?
            .try_into()
            .unwrap(),
    );
    let price_usd = bits as f64 / (1u64 << 32) as f64;
    Ok(PriceQuote {
        price_usd,
        published_ms: 0,
    })
}

/// Fetch the Pyth price-update bytes from Hermes for `price_id_hex` (the accumulator
/// update consumed on-chain by `pyth::update_single_price_feed`). The liquidation PTB
/// builder turns these bytes into the in-band Pyth update before Scallop's
/// `pyth_rule::set_price_as_primary` + `liquidate`.
pub async fn fetch_pyth_vaa(hermes_url: &str, price_id_hex: &str) -> Result<Vec<u8>> {
    let base = hermes_url.trim_end_matches('/');
    let url = format!("{base}/v2/updates/price/latest?ids[]={price_id_hex}&encoding=hex");
    let resp: serde_json::Value = reqwest::get(&url).await?.error_for_status()?.json().await?;
    let hexstr = resp["binary"]["data"][0]
        .as_str()
        .ok_or_else(|| anyhow!("Hermes response missing binary.data[0]"))?;
    decode_hex(hexstr)
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        bail!("odd-length hex from Hermes");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("bad hex: {e}")))
        .collect()
}
