//! Core data model for pools and routes.
//!
//! One `PoolState` per **(pair, venue, fee tier)** edge. `PoolKind` carries either
//! constant-product reserves (the testnet-validated `amm.rs` path, unchanged) or
//! concentrated-liquidity state (`clmm.rs`). The scanner dispatches on the kind; the
//! V2 arithmetic stays bit-for-bit identical to `sources/math.move`.

use serde::{Deserialize, Serialize};

use crate::clmm::ClmmState;

/// Fully-qualified coin type tag, e.g. `0x2::sui::SUI`.
pub type TokenId = String;

/// On-chain object id (hex) of a pool.
pub type PoolId = String;

/// Which venue a pool belongs to — selects the adapter used when building the PTB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dex {
    /// In-package reference AMM (`arbitrage_system::amm_v2`).
    AmmV2,
    Cetus,
    Turbos,
}

/// Pricing model + the state each model needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PoolKind {
    /// Constant-product pool — priced by `amm::get_amount_out`.
    V2 {
        reserve_a: u64,
        reserve_b: u64,
        /// Swap fee in basis points (30 = 0.30%).
        fee_bps: u64,
    },
    /// Concentrated-liquidity pool — priced by `clmm::quote_exact_in`. Fee lives in
    /// `ClmmState::fee_pips` (1e6 = 100%).
    Clmm(ClmmState),
}

/// A snapshot of one pool. `token_a`/`token_b` follow the on-chain type-arg order
/// (`token_a` = token0). For CLMM, swapping `token_a -> token_b` is the engine's
/// `a_to_b = true` (price decreasing).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolState {
    pub id: PoolId,
    pub dex: Dex,
    pub token_a: TokenId,
    pub token_b: TokenId,
    pub kind: PoolKind,
    /// On-chain object version / checkpoint seq of this snapshot. Drives staleness +
    /// gap detection during live ingestion (0 for synthetic/test pools).
    pub last_seq: u64,
}

impl PoolState {
    /// Construct a constant-product pool.
    #[must_use]
    pub fn v2(
        id: impl Into<PoolId>,
        dex: Dex,
        token_a: impl Into<TokenId>,
        token_b: impl Into<TokenId>,
        reserve_a: u64,
        reserve_b: u64,
        fee_bps: u64,
    ) -> Self {
        Self {
            id: id.into(),
            dex,
            token_a: token_a.into(),
            token_b: token_b.into(),
            kind: PoolKind::V2 {
                reserve_a,
                reserve_b,
                fee_bps,
            },
            last_seq: 0,
        }
    }

    /// Construct a concentrated-liquidity pool.
    #[must_use]
    pub fn clmm(
        id: impl Into<PoolId>,
        dex: Dex,
        token_a: impl Into<TokenId>,
        token_b: impl Into<TokenId>,
        state: ClmmState,
    ) -> Self {
        Self {
            id: id.into(),
            dex,
            token_a: token_a.into(),
            token_b: token_b.into(),
            kind: PoolKind::Clmm(state),
            last_seq: 0,
        }
    }

    /// V2 reserves oriented for a swap *from* `token_in`: `(reserve_in, reserve_out)`.
    /// `None` if `token_in` is not in this pool or this is not a V2 pool.
    #[must_use]
    pub fn reserves_from(&self, token_in: &str) -> Option<(u64, u64)> {
        let PoolKind::V2 {
            reserve_a,
            reserve_b,
            ..
        } = &self.kind
        else {
            return None;
        };
        if token_in == self.token_a {
            Some((*reserve_a, *reserve_b))
        } else if token_in == self.token_b {
            Some((*reserve_b, *reserve_a))
        } else {
            None
        }
    }

    /// The concentrated-liquidity state, if this is a CLMM pool.
    #[must_use]
    pub fn clmm_state(&self) -> Option<&ClmmState> {
        match &self.kind {
            PoolKind::Clmm(s) => Some(s),
            PoolKind::V2 { .. } => None,
        }
    }

    /// The other token of the pair relative to `token_in`.
    #[must_use]
    pub fn other(&self, token_in: &str) -> Option<&TokenId> {
        if token_in == self.token_a {
            Some(&self.token_b)
        } else if token_in == self.token_b {
            Some(&self.token_a)
        } else {
            None
        }
    }
}
