//! Stage-1 liquidation sizing → `Opportunity`.
//!
//! For an underwater obligation, size the repay so the seized collateral, **after the
//! swap-back slippage** (priced with the CLMM engine), still clears the flash fee, gas,
//! and `min_profit`. A fat liquidation bonus can be entirely eaten by slippage when you
//! seize+dump a large collateral amount, so we sweep repay fractions and keep the best
//! net — exactly the discipline the arb scanner uses for trade sizing.
//!
//! This is the local pre-filter. The repay actually submitted is re-derived from the
//! protocol's authoritative `calculate_liquidation_amounts` read (live path); this
//! sizer decides whether an obligation is worth that read + builds the `Opportunity`.

use super::types::{AssetParams, Obligation};
use crate::clmm::{self, ClmmState};
use crate::flashloan::quote_fee_bps;
use crate::scanner::{Hop, LiquidationLeg, OppKind, Opportunity};
use crate::types::Dex;

/// The CLMM pool used to swap seized collateral back to the debt asset.
pub struct SwapBack<'a> {
    pub pool_id: String,
    pub dex: Dex,
    /// True if collateral is the pool's `token_a` (collateral→debt is then a→b).
    pub a_to_b: bool,
    pub state: &'a ClmmState,
}

/// Liquidation sizing knobs (per protocol/market; verified values come from chain).
#[derive(Clone, Debug)]
pub struct LiqParams {
    /// Max fraction of the debt repayable in one call (close factor, e.g. 0.2).
    pub close_factor: f64,
    /// Liquidation bonus / collateral discount (e.g. 0.05 = seize 5% extra value).
    pub liquidation_bonus: f64,
    /// Flash-loan fee on the repay (bps); 0 if repaying from owned capital.
    pub flash_fee_bps: u64,
    /// Gas estimate in debt-asset raw units.
    pub gas_cost: u64,
    /// Minimum net profit (debt-asset raw units) to emit an opportunity.
    pub min_profit: u64,
    /// Repay fractions of the close-factor max to sweep (slippage may favor smaller).
    pub candidate_fractions: Vec<f64>,
}

/// Raw collateral units seized for repaying `repay_raw` of debt, including the bonus,
/// capped at the obligation's available collateral.
#[must_use]
pub fn seized_for_repay(
    repay_raw: u64,
    debt: &AssetParams,
    collateral: &AssetParams,
    bonus: f64,
    collateral_available_raw: u64,
) -> u64 {
    let repay_value_usd = debt.value_usd(repay_raw);
    let seized_value_usd = repay_value_usd * (1.0 + bonus);
    let seized =
        (seized_value_usd / collateral.price_usd) * 10f64.powi(i32::from(collateral.decimals));
    (seized as u64).min(collateral_available_raw)
}

/// Size a single `(debt, collateral)` liquidation against the swap-back pool. Returns a
/// `Liquidation` opportunity if some repay fraction nets ≥ `min_profit`.
#[must_use]
pub fn size_liquidation(
    ob: &Obligation,
    debt: &AssetParams,
    collateral: &AssetParams,
    swap: &SwapBack,
    lp: &LiqParams,
) -> Option<Opportunity> {
    let debt_pos = ob.debt(&debt.coin_type)?;
    let coll_pos = ob.collateral(&collateral.coin_type)?;
    let max_repay = (debt_pos.amount as f64 * lp.close_factor) as u64;
    if max_repay == 0 {
        return None;
    }

    let mut best: Option<Opportunity> = None;
    for &frac in &lp.candidate_fractions {
        let repay = ((max_repay as f64) * frac) as u64;
        if repay == 0 {
            continue;
        }
        let seized = seized_for_repay(
            repay,
            debt,
            collateral,
            lp.liquidation_bonus,
            coll_pos.amount,
        );
        if seized == 0 {
            continue;
        }
        // Swap seized collateral back to the debt asset (the engine prices slippage).
        let Some(debt_out) = clmm::quote_exact_in(swap.state, seized, swap.a_to_b) else {
            continue;
        };
        let flash_fee = quote_fee_bps(repay, lp.flash_fee_bps);
        let cost = repay.saturating_add(flash_fee).saturating_add(lp.gas_cost);
        let net = debt_out.saturating_sub(cost);
        if net < lp.min_profit {
            continue;
        }
        if best.as_ref().is_none_or(|b| net > b.net_profit) {
            best = Some(opportunity(
                ob, debt, collateral, swap, repay, debt_out, flash_fee, net,
            ));
        }
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn opportunity(
    ob: &Obligation,
    debt: &AssetParams,
    collateral: &AssetParams,
    swap: &SwapBack,
    repay: u64,
    debt_out: u64,
    flash_fee: u64,
    net: u64,
) -> Opportunity {
    let hop = Hop {
        pool_id: swap.pool_id.clone(),
        dex: swap.dex,
        token_in: collateral.coin_type.clone(),
        token_out: debt.coin_type.clone(),
        a_to_b: swap.a_to_b,
    };
    let leg = LiquidationLeg {
        protocol: ob.protocol,
        obligation_id: ob.id.clone(),
        debt_type: debt.coin_type.clone(),
        collateral_type: collateral.coin_type.clone(),
        repay_amount: repay,
        extra_object_ids: Vec::new(), // filled by the live PTB assembler (market, x_oracle, …)
    };
    // start == end == debt asset; profit semantics identical to arb.
    Opportunity {
        kind: OppKind::Liquidation,
        liquidation: Some(leg),
        route: vec![hop],
        input_amount: repay,
        output_amount: debt_out,
        gross_profit: debt_out.saturating_sub(repay),
        flash_fee,
        net_profit: net,
        // record seized for logging/analysis via the route; debt_out already reflects it
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clmm::Q64;
    use crate::liquidation::types::Position;
    use crate::scanner::Protocol;

    fn ap(coin: &str, dec: u8, price: f64) -> AssetParams {
        AssetParams {
            coin_type: coin.into(),
            decimals: dec,
            price_usd: price,
            liquidation_threshold: 0.7,
            borrow_weight: 1.0,
        }
    }

    // Underwater obligation: SUI collateral, USDC debt.
    fn ob() -> Obligation {
        Obligation {
            id: "0xob".into(),
            protocol: Protocol::Scallop,
            collaterals: vec![Position {
                coin_type: "SUI".into(),
                amount: 1_000_000_000_000,
            }], // 1000 SUI
            debts: vec![Position {
                coin_type: "USDC".into(),
                amount: 1_000_000_000_000,
            }], // 1000 USDC@9
        }
    }

    fn lp(bonus: f64, min_profit: u64) -> LiqParams {
        LiqParams {
            close_factor: 0.2,
            liquidation_bonus: bonus,
            flash_fee_bps: 0,
            gas_cost: 0,
            min_profit,
            candidate_fractions: vec![1.0, 0.5, 0.25],
        }
    }

    // Deep ~1:1 SUI/USDC-ish pool so swap slippage is small (single range, fee 0.05%).
    fn deep_pool() -> ClmmState {
        clmm::single_range(Q64, 100_000_000_000_000_000, 500)
    }

    #[test]
    fn profitable_liquidation_is_sized() {
        let debt = ap("USDC", 9, 1.0);
        let coll = ap("SUI", 9, 1.0); // price 1.0 each for a clean ~1:1 swap
        let pool = deep_pool();
        let swap = SwapBack {
            pool_id: "0xPOOL".into(),
            dex: Dex::Cetus,
            a_to_b: true,
            state: &pool,
        };
        let opp = size_liquidation(&ob(), &debt, &coll, &swap, &lp(0.08, 1)).expect("profitable");
        assert_eq!(opp.kind, OppKind::Liquidation);
        let leg = opp.liquidation.as_ref().unwrap();
        assert_eq!(leg.protocol, Protocol::Scallop);
        assert_eq!(leg.debt_type, "USDC");
        assert!(opp.net_profit > 0);
        assert!(opp.output_amount > opp.input_amount); // bonus survives slippage
        assert_eq!(opp.route.len(), 1);
    }

    #[test]
    fn zero_bonus_is_not_profitable() {
        let debt = ap("USDC", 9, 1.0);
        let coll = ap("SUI", 9, 1.0);
        let pool = deep_pool();
        let swap = SwapBack {
            pool_id: "0xPOOL".into(),
            dex: Dex::Cetus,
            a_to_b: true,
            state: &pool,
        };
        // No bonus + swap fee ⇒ you get back less than you repaid.
        assert!(size_liquidation(&ob(), &debt, &coll, &swap, &lp(0.0, 1)).is_none());
    }

    #[test]
    fn thin_pool_slippage_eats_the_bonus() {
        let debt = ap("USDC", 9, 1.0);
        let coll = ap("SUI", 9, 1.0);
        // Shallow pool: dumping seized collateral craters the price → bonus eaten.
        let thin = clmm::single_range(Q64, 1_000_000_000, 500);
        let swap = SwapBack {
            pool_id: "0xPOOL".into(),
            dex: Dex::Cetus,
            a_to_b: true,
            state: &thin,
        };
        assert!(size_liquidation(&ob(), &debt, &coll, &swap, &lp(0.08, 1)).is_none());
    }

    #[test]
    fn seized_includes_bonus_and_caps_at_collateral() {
        let debt = ap("USDC", 9, 1.0);
        let coll = ap("SUI", 9, 1.0);
        // repay 100 USDC, 10% bonus → ~110 SUI worth seized.
        let seized = seized_for_repay(100_000_000_000, &debt, &coll, 0.10, u64::MAX);
        assert!((109_000_000_000..=111_000_000_000).contains(&seized));
        // cap: never seize more than available collateral
        let capped = seized_for_repay(100_000_000_000, &debt, &coll, 0.10, 5_000_000_000);
        assert_eq!(capped, 5_000_000_000);
    }
}
