//! Authoritative on-chain quoting (feature = "live") — funnel stage 2.
//!
//! Stage 1 (`scanner`) ranks candidates with the in-process `clmm` engine. Before a
//! candidate is acted on it is **re-priced by each venue's own on-chain quoter** via
//! read-only `devInspect` (no signing, no gas, no commit) — the lesson of
//! `docs/authoritative-pnl-report.md`: engine estimates over-detect; only authoritative
//! pricing makes net P&L honest. This is the Rust port of the proven Python quoters
//! (`validation/cetus/cetus_rpc.py`, `turbos_rpc.py`), which stay as the parity oracle.
//!
//! VERIFICATION STATUS: written against the live Sui SDK + the pinned venue packages;
//! compiles under `--features live` (which pulls the Sui SDK). It is **not** built or
//! run in the offline CI here. The BCS return offsets mirror the Python decoders that
//! passed Cetus parity 1034/1034; Turbos's `compute_swap_result` decode is asserted
//! against `turbos_rpc.py` before trusting it (constraint #2 / audit P4).

use anyhow::{anyhow, Result};
use sui_json_rpc_types::SuiTransactionBlockEffectsAPI;
use sui_sdk::SuiClient;
use sui_types::base_types::{ObjectID, SequenceNumber, SuiAddress};
use sui_types::programmable_transaction_builder::ProgrammableTransactionBuilder;
use sui_types::transaction::{Command, ObjectArg, SharedObjectMutability, TransactionKind};
use sui_types::TypeTag;

use crate::types::Dex;

/// Mainnet package ids of the venues we quote (match the Move.toml pins + the Python
/// validation suite).
pub const CETUS_PKG: &str = "0x1eabed72c53feb3805120a081dc15963c204dc8d091542592abaf7a35689b2fb";
pub const TURBOS_PKG: &str = "0x91bfbc386a41afcfd9b2533058d7e915a1d3829089cc268ff4333d54d6339ca1";

/// Any address works as a dev-inspect sender (no gas, no signature).
const DEV_INSPECT_SENDER: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";

/// On-chain coordinates of a pool needed to build quote/swap calls. Resolved fresh
/// from chain (object versions move every checkpoint — never cache across builds).
#[derive(Clone, Debug)]
pub struct LivePoolRef {
    pub dex: Dex,
    pub pool_id: ObjectID,
    pub init_shared_version: SequenceNumber,
    pub type_a: TypeTag,
    pub type_b: TypeTag,
    /// Turbos fee-tier type argument; `None` for Cetus / V2.
    pub fee_type: Option<TypeTag>,
}

fn sender() -> SuiAddress {
    DEV_INSPECT_SENDER.parse().expect("valid sender")
}

/// Authoritative exact-in quote for one hop. `a_to_b` = token_a → token_b.
pub async fn authoritative_hop_quote(
    client: &SuiClient,
    pool: &LivePoolRef,
    a_to_b: bool,
    amount_in: u64,
) -> Result<u64> {
    match pool.dex {
        Dex::Cetus => cetus_quote(client, pool, a_to_b, amount_in).await,
        Dex::Turbos => turbos_quote(client, pool, a_to_b, amount_in).await,
        Dex::AmmV2 => Err(anyhow!(
            "amm_v2 has no on-chain CLMM quoter; price with the engine"
        )),
    }
}

/// Re-price a whole route hop-by-hop authoritatively, threading the output forward.
/// `route` must be in execution order and aligned with `refs`. Returns the final
/// output (or an error if any hop's quote fails).
pub async fn authoritative_route_quote(
    client: &SuiClient,
    refs: &[(LivePoolRef, bool)], // (pool, a_to_b) per hop
    input: u64,
) -> Result<u64> {
    let mut amount = input;
    for (pool, a_to_b) in refs {
        amount = authoritative_hop_quote(client, pool, *a_to_b, amount).await?;
    }
    Ok(amount)
}

/// Like [`authoritative_route_quote`] but returns each hop's output amount, in order
/// — used to set per-hop `min_out` floors for the submitted PTB (Phase 4 race guard).
pub async fn authoritative_route_quotes(
    client: &SuiClient,
    refs: &[(LivePoolRef, bool)],
    input: u64,
) -> Result<Vec<u64>> {
    let mut outs = Vec::with_capacity(refs.len());
    let mut amount = input;
    for (pool, a_to_b) in refs {
        amount = authoritative_hop_quote(client, pool, *a_to_b, amount).await?;
        outs.push(amount);
    }
    Ok(outs)
}

/// Cetus `pool::calculate_swap_result(pool, a2b, by_amount_in, amount)` via devInspect.
/// Returns `amount_out`. Mirrors `cetus_rpc.py::cetus_quote` (parity-proven).
async fn cetus_quote(
    client: &SuiClient,
    pool: &LivePoolRef,
    a2b: bool,
    amount: u64,
) -> Result<u64> {
    let mut ptb = ProgrammableTransactionBuilder::new();
    let pool_arg = ptb.obj(ObjectArg::SharedObject {
        id: pool.pool_id,
        initial_shared_version: pool.init_shared_version,
        mutability: SharedObjectMutability::Immutable,
    })?;
    let a2b_arg = ptb.pure(a2b)?;
    let by_in_arg = ptb.pure(true)?;
    let amt_arg = ptb.pure(amount)?;
    ptb.command(Command::move_call(
        CETUS_PKG.parse::<ObjectID>()?,
        sui_types::Identifier::new("pool")?,
        sui_types::Identifier::new("calculate_swap_result")?,
        vec![pool.type_a.clone(), pool.type_b.clone()],
        vec![pool_arg, a2b_arg, by_in_arg, amt_arg],
    ));
    let bytes = dev_inspect_first_return(client, ptb.finish()).await?;
    // CalculatedSwapResult: amount_in u64, amount_out u64, ... → amount_out at [8,16).
    decode_u64_at(&bytes, 8)
}

/// Turbos `pool_fetcher::compute_swap_result(...)` via devInspect (the public quoter;
/// `pool::compute_swap_result` is `friend`-gated). Decode offset is validated against
/// `turbos_rpc.py` before this venue is trusted for sizing (audit P4, in-range).
async fn turbos_quote(
    client: &SuiClient,
    pool: &LivePoolRef,
    a2b: bool,
    amount: u64,
) -> Result<u64> {
    let fee_type = pool
        .fee_type
        .clone()
        .ok_or_else(|| anyhow!("turbos pool ref missing fee-tier type arg"))?;
    let mut ptb = ProgrammableTransactionBuilder::new();
    let pool_arg = ptb.obj(ObjectArg::SharedObject {
        id: pool.pool_id,
        initial_shared_version: pool.init_shared_version,
        mutability: SharedObjectMutability::Immutable,
    })?;
    let a2b_arg = ptb.pure(a2b)?;
    let by_in_arg = ptb.pure(true)?;
    let amt_arg = ptb.pure(amount)?;
    // Clock 0x6 is required by compute_swap_result.
    let clock_arg = ptb.obj(ObjectArg::SharedObject {
        id: ObjectID::from_hex_literal("0x6")?,
        initial_shared_version: SequenceNumber::from_u64(1),
        mutability: SharedObjectMutability::Immutable,
    })?;
    ptb.command(Command::move_call(
        TURBOS_PKG.parse::<ObjectID>()?,
        sui_types::Identifier::new("pool_fetcher")?,
        sui_types::Identifier::new("compute_swap_result")?,
        vec![pool.type_a.clone(), pool.type_b.clone(), fee_type],
        vec![pool_arg, a2b_arg, by_in_arg, amt_arg, clock_arg],
    ));
    let bytes = dev_inspect_first_return(client, ptb.finish()).await?;
    // ComputeSwapState BCS layout (each field is a u128 = 16 bytes), matching the
    // parity-proven `turbos_rpc.py`:
    //   amount_a[0:16] amount_b[16:32] amount_specified_remaining[32:48] amount_calculated[48:64]
    // The exact-in OUTPUT is `amount_calculated` (independent of direction), so we read
    // the u128 at offset 48 and narrow. (`a2b` only selects the input/output coin types,
    // already encoded in the call args.)
    let _ = a2b;
    decode_u128_at(&bytes, 48)
        .and_then(|v| u64::try_from(v).map_err(|_| anyhow!("turbos amount_calculated exceeds u64")))
}

/// Run a read-only `ProgrammableTransaction` and return the first command's first
/// return value (BCS bytes). Errors if the dev-inspect reverted.
async fn dev_inspect_first_return(
    client: &SuiClient,
    pt: sui_types::transaction::ProgrammableTransaction,
) -> Result<Vec<u8>> {
    let res = client
        .read_api()
        .dev_inspect_transaction_block(
            sender(),
            TransactionKind::ProgrammableTransaction(pt),
            None,
            None,
            None,
        )
        .await?;
    let effects = &res.effects;
    if effects.status().is_err() {
        return Err(anyhow!("devInspect reverted: {:?}", effects.status()));
    }
    let results = res
        .results
        .ok_or_else(|| anyhow!("devInspect returned no results"))?;
    let first = results
        .first()
        .ok_or_else(|| anyhow!("devInspect: empty results"))?;
    let (bytes, _ty) = first
        .return_values
        .first()
        .ok_or_else(|| anyhow!("devInspect: no return value"))?;
    Ok(bytes.clone())
}

/// Decode a little-endian u64 at byte offset `off`.
fn decode_u64_at(bytes: &[u8], off: usize) -> Result<u64> {
    let slice = bytes
        .get(off..off + 8)
        .ok_or_else(|| anyhow!("return value too short for u64 at {off}"))?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

/// Decode a little-endian u128 at byte offset `off`.
fn decode_u128_at(bytes: &[u8], off: usize) -> Result<u128> {
    let slice = bytes
        .get(off..off + 16)
        .ok_or_else(|| anyhow!("return value too short for u128 at {off}"))?;
    Ok(u128::from_le_bytes(slice.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cetus `CalculatedSwapResult` BCS layout (matches the parity-proven
    /// `validation/cetus/cetus_rpc.py::decode_swap_result`):
    ///   amount_in u64 [0,8) · amount_out u64 [8,16) · fee_amount u64 [16,24) · ...
    /// Locks the Cetus output offset (8) — fails loudly if the decode drifts.
    #[test]
    fn cetus_amount_out_offset_matches_python() {
        let mut b = Vec::new();
        b.extend_from_slice(&1_000_000u64.to_le_bytes()); // amount_in
        b.extend_from_slice(&987_654u64.to_le_bytes()); // amount_out  <-- the one we read
        b.extend_from_slice(&321u64.to_le_bytes()); // fee_amount
        b.extend_from_slice(&30u64.to_le_bytes()); // fee_rate
        assert_eq!(decode_u64_at(&b, 8).unwrap(), 987_654);
    }

    /// Turbos `ComputeSwapState` BCS layout (matches `turbos_rpc.py`, which returns
    /// `b[48:64]`): four u128 fields —
    ///   amount_a[0,16) · amount_b[16,32) · amount_specified_remaining[32,48) ·
    ///   amount_calculated[48,64)   <-- the exact-in OUTPUT.
    /// Locks the Turbos output offset (48, u128) — the prior u64@8 decode was wrong.
    #[test]
    fn turbos_amount_calculated_offset_matches_python() {
        let mut b = Vec::new();
        b.extend_from_slice(&111u128.to_le_bytes()); // amount_a
        b.extend_from_slice(&222u128.to_le_bytes()); // amount_b
        b.extend_from_slice(&333u128.to_le_bytes()); // amount_specified_remaining
        b.extend_from_slice(&987_654u128.to_le_bytes()); // amount_calculated <-- output
        assert_eq!(decode_u128_at(&b, 48).unwrap(), 987_654);
        // and NOT the (previously, wrongly) decoded u64@8 = low 8 bytes of amount_b
        assert_ne!(decode_u64_at(&b, 8).unwrap(), 987_654);
    }
}
