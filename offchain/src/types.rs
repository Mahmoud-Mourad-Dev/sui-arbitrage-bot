//! Core data model for pools and routes.

use serde::{Deserialize, Serialize};

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

/// A snapshot of a single constant-product pool's state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolState {
    pub id: PoolId,
    pub dex: Dex,
    pub token_a: TokenId,
    pub token_b: TokenId,
    pub reserve_a: u64,
    pub reserve_b: u64,
    /// Swap fee in basis points (30 = 0.30%).
    pub fee_bps: u64,
}

impl PoolState {
    /// Reserves oriented for a swap *from* `token_in`: `(reserve_in, reserve_out)`.
    /// Returns `None` if `token_in` is not in this pool.
    #[must_use]
    pub fn reserves_from(&self, token_in: &str) -> Option<(u64, u64)> {
        if token_in == self.token_a {
            Some((self.reserve_a, self.reserve_b))
        } else if token_in == self.token_b {
            Some((self.reserve_b, self.reserve_a))
        } else {
            None
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
