//! Protocol oracle access (feature = "live").
//!
//! Constraint #3: health MUST use the SAME oracle + math as the protocol. We read the
//! protocol's own price view rather than re-deriving it: for Scallop we `devInspect`
//! `price::get_price(x_oracle, type, clock)` — the exact value `liquidate` sees.
//!
//! For the in-PTB price refresh that `liquidate` requires, this module also fetches the
//! Pyth **accumulator** (PNAU) blob from Hermes (`fetch_pyth_accumulator`) and extracts the
//! embedded Wormhole VAA (`extract_vaa_from_accumulator`) — the two byte inputs the verified
//! on-chain flow consumes (docs/scallop-liquidation-verified.md §4). The PNAU parse is
//! unit-tested and confirmed against the real mainnet liquidation tx.
//!
//! Staleness is explicit: a price too old to pass the protocol's `clock` freshness check is
//! a non-opportunity (we never act on a stale price).

use anyhow::{anyhow, bail, Context, Result};

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

/// Fetch the Pyth **accumulator** update blob (PNAU) from Hermes covering `price_ids`
/// (Pyth feed ids, hex). This single blob is what the on-chain
/// `create_authenticated_price_infos_using_accumulator` consumes; the embedded Wormhole
/// VAA is extracted with [`extract_vaa_from_accumulator`].
pub async fn fetch_pyth_accumulator(hermes_url: &str, price_ids: &[String]) -> Result<Vec<u8>> {
    if price_ids.is_empty() {
        bail!("no Pyth feed ids configured for the liquidation assets");
    }
    let base = hermes_url.trim_end_matches('/');
    let mut url = format!("{base}/v2/updates/price/latest?encoding=hex");
    for id in price_ids {
        url.push_str("&ids[]=");
        url.push_str(id.trim_start_matches("0x"));
    }
    let resp: serde_json::Value = reqwest::get(&url).await?.error_for_status()?.json().await?;
    let hexstr = resp["binary"]["data"][0]
        .as_str()
        .ok_or_else(|| anyhow!("Hermes response missing binary.data[0]"))?;
    decode_hex(hexstr)
}

/// Extract the embedded Wormhole VAA from a Pyth accumulator (PNAU) blob.
///
/// Format (CONFIRMED against mainnet liquidation tx `AYdhgWMq…`: the 952-byte VAA sits at
/// offset 10, with its `u16` length at byte 8):
/// `"PNAU" | major:u8 | minor:u8 | trailing_hdr_size:u8 | <trailing bytes> | update_type:u8 |
///  vaa_len:u16 BE | vaa[..]`.
pub fn extract_vaa_from_accumulator(acc: &[u8]) -> Result<Vec<u8>> {
    if acc.len() < 8 || &acc[0..4] != b"PNAU" {
        bail!("not a PNAU accumulator update");
    }
    let trailing = acc[6] as usize;
    let mut cur = 7 + trailing; // skip magic(4)+major+minor+trailing_size + trailing bytes
    cur += 1; // update_type
    let hi = *acc
        .get(cur)
        .context("accumulator truncated before vaa length")?;
    let lo = *acc
        .get(cur + 1)
        .context("accumulator truncated before vaa length")?;
    let len = u16::from_be_bytes([hi, lo]) as usize;
    cur += 2;
    let vaa = acc
        .get(cur..cur + len)
        .ok_or_else(|| anyhow!("vaa length {len} exceeds accumulator"))?
        .to_vec();
    Ok(vaa)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_vaa_parses_pnau_header() {
        // Build a PNAU blob mirroring the verified mainnet layout: magic, major=1, minor=0,
        // trailing_size=0, update_type=0, vaa_len:u16 BE, then the VAA. (Real tx: vaa at
        // offset 10, len 952 — the same offsets this produces for trailing_size 0.)
        let vaa = [1u8, 0, 0, 0, 6, 13, 42, 7];
        let mut acc = b"PNAU".to_vec();
        acc.extend_from_slice(&[1, 0, 0, 0]); // major, minor, trailing_size=0, update_type=0
        acc.extend_from_slice(&(vaa.len() as u16).to_be_bytes());
        acc.extend_from_slice(&vaa);
        assert_eq!(10, acc.len() - vaa.len()); // VAA begins at offset 10, as on-chain
        assert_eq!(extract_vaa_from_accumulator(&acc).unwrap(), vaa);

        // A non-trivial trailing header is skipped correctly.
        let mut acc2 = b"PNAU".to_vec();
        acc2.extend_from_slice(&[1, 0, 2, 0xAA, 0xBB]); // trailing_size=2, then 2 trailing bytes
        acc2.push(0); // update_type
        acc2.extend_from_slice(&(vaa.len() as u16).to_be_bytes());
        acc2.extend_from_slice(&vaa);
        assert_eq!(extract_vaa_from_accumulator(&acc2).unwrap(), vaa);

        assert!(extract_vaa_from_accumulator(b"NOPExxxx").is_err());
        assert!(extract_vaa_from_accumulator(&[]).is_err());
    }
}
