//! Lending-obligation data model (protocol-agnostic where possible).

use crate::scanner::Protocol;
use crate::types::TokenId;

/// A coin position in an obligation — collateral or debt, in raw on-chain units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub coin_type: TokenId,
    pub amount: u64,
}

/// Per-asset risk parameters, from the protocol's market config + oracle.
#[derive(Clone, Debug)]
pub struct AssetParams {
    pub coin_type: TokenId,
    pub decimals: u8,
    /// USD price from the protocol's oracle (the SAME oracle the protocol reads).
    pub price_usd: f64,
    /// Liquidation threshold / collateral weight for liq math (0..1).
    pub liquidation_threshold: f64,
    /// Borrow weight applied to debt (>= 1).
    pub borrow_weight: f64,
}

impl AssetParams {
    /// USD value of `amount_raw` of this asset.
    #[must_use]
    pub fn value_usd(&self, amount_raw: u64) -> f64 {
        (amount_raw as f64 / 10f64.powi(i32::from(self.decimals))) * self.price_usd
    }
}

/// A lending obligation snapshot (one entry in the off-chain index).
#[derive(Clone, Debug)]
pub struct Obligation {
    pub id: String,
    pub protocol: Protocol,
    pub collaterals: Vec<Position>,
    pub debts: Vec<Position>,
}

impl Obligation {
    #[must_use]
    pub fn debt(&self, coin_type: &str) -> Option<&Position> {
        self.debts.iter().find(|p| p.coin_type == coin_type)
    }
    #[must_use]
    pub fn collateral(&self, coin_type: &str) -> Option<&Position> {
        self.collaterals.iter().find(|p| p.coin_type == coin_type)
    }
}
