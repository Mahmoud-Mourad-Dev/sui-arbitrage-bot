//! Stage-1 LOCAL health approximation.
//!
//! Health factor = (liquidation-weighted collateral) / (borrow-weighted debt). HF < 1
//! ⇒ liquidatable. This is a cheap, **over-including** pre-filter to shortlist
//! candidates; the AUTHORITATIVE verdict + sizing is the protocol's own on-chain read
//! (`detect`'s live path), so a slightly-wrong local HF never misfires capital — it
//! only decides which obligations are worth an authoritative devInspect.
//!
//! Parity discipline: this approximation is checked against the protocol's on-chain
//! health view across sampled obligations (the `validation/*_rpc.py` style); any
//! disagreement just means we widen the local margin, never that we act on the local
//! number.

use std::collections::HashMap;

use super::types::{AssetParams, Obligation};

/// Σ debt_usd · borrow_weight.
#[must_use]
pub fn weighted_debt_usd(ob: &Obligation, params: &HashMap<String, AssetParams>) -> f64 {
    ob.debts
        .iter()
        .filter_map(|p| {
            params
                .get(&p.coin_type)
                .map(|ap| ap.value_usd(p.amount) * ap.borrow_weight)
        })
        .sum()
}

/// Σ collateral_usd · liquidation_threshold.
#[must_use]
pub fn liq_weighted_collateral_usd(ob: &Obligation, params: &HashMap<String, AssetParams>) -> f64 {
    ob.collaterals
        .iter()
        .filter_map(|p| {
            params
                .get(&p.coin_type)
                .map(|ap| ap.value_usd(p.amount) * ap.liquidation_threshold)
        })
        .sum()
}

/// Health factor. `None` when the obligation has no (priced) debt — not liquidatable.
#[must_use]
pub fn health_factor(ob: &Obligation, params: &HashMap<String, AssetParams>) -> Option<f64> {
    let debt = weighted_debt_usd(ob, params);
    if debt <= 0.0 {
        return None;
    }
    Some(liq_weighted_collateral_usd(ob, params) / debt)
}

/// Local pre-filter verdict: HF < 1 (with a configurable margin to over-include, since
/// the authoritative read makes the final call). `margin = 0.02` shortlists anything
/// within 2% above the threshold too.
#[must_use]
pub fn is_liquidatable(
    ob: &Obligation,
    params: &HashMap<String, AssetParams>,
    margin: f64,
) -> bool {
    matches!(health_factor(ob, params), Some(hf) if hf < 1.0 + margin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquidation::types::Position;
    use crate::scanner::Protocol;

    fn ap(coin: &str, decimals: u8, price: f64, lt: f64, bw: f64) -> AssetParams {
        AssetParams {
            coin_type: coin.into(),
            decimals,
            price_usd: price,
            liquidation_threshold: lt,
            borrow_weight: bw,
        }
    }

    fn params() -> HashMap<String, AssetParams> {
        // SUI: $1.50, 9 decimals, 0.7 liq threshold; USDC: $1, 6 decimals, borrow weight 1.
        HashMap::from([
            ("SUI".to_string(), ap("SUI", 9, 1.50, 0.70, 1.0)),
            ("USDC".to_string(), ap("USDC", 6, 1.00, 0.80, 1.0)),
        ])
    }

    fn obligation(coll_sui: u64, debt_usdc: u64) -> Obligation {
        Obligation {
            id: "0xob".into(),
            protocol: Protocol::Scallop,
            collaterals: vec![Position {
                coin_type: "SUI".into(),
                amount: coll_sui,
            }],
            debts: vec![Position {
                coin_type: "USDC".into(),
                amount: debt_usdc,
            }],
        }
    }

    #[test]
    fn healthy_obligation_not_liquidatable() {
        // 1000 SUI ($1500) × 0.7 = $1050 weighted collateral vs $500 debt → HF 2.1.
        let ob = obligation(1_000_000_000_000, 500_000_000);
        let hf = health_factor(&ob, &params()).unwrap();
        assert!(hf > 2.0);
        assert!(!is_liquidatable(&ob, &params(), 0.0));
    }

    #[test]
    fn underwater_obligation_is_liquidatable() {
        // 1000 SUI ($1500) × 0.7 = $1050 weighted collateral vs $1200 debt → HF 0.875.
        let ob = obligation(1_000_000_000_000, 1_200_000_000);
        let hf = health_factor(&ob, &params()).unwrap();
        assert!(hf < 1.0);
        assert!(is_liquidatable(&ob, &params(), 0.0));
    }

    #[test]
    fn no_debt_is_not_liquidatable() {
        let mut ob = obligation(1_000_000_000_000, 0);
        ob.debts.clear();
        assert_eq!(health_factor(&ob, &params()), None);
        assert!(!is_liquidatable(&ob, &params(), 0.0));
    }

    #[test]
    fn margin_widens_the_shortlist() {
        // HF just above 1 (≈1.01): excluded at margin 0, included at margin 0.02.
        // 1000 SUI×0.7=$1050 vs debt $1039.6 → HF≈1.01.
        let ob = obligation(1_000_000_000_000, 1_039_600_000);
        let hf = health_factor(&ob, &params()).unwrap();
        assert!(hf > 1.0 && hf < 1.02);
        assert!(!is_liquidatable(&ob, &params(), 0.0));
        assert!(is_liquidatable(&ob, &params(), 0.02));
    }
}
