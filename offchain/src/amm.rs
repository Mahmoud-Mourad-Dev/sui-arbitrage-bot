//! Constant-product (x*y=k) swap math.
//!
//! Kept identical to `sources/math.move` so off-chain simulation predicts
//! on-chain execution exactly. All intermediate math is `u128`; inputs/outputs
//! are `u64` (Sui coin amounts). Functions return `None` on degenerate input
//! instead of panicking, so the hot scan path never aborts.

/// Fee denominator: fees are basis points of 1/10_000 (30 = 0.30%).
pub const FEE_DENOM: u64 = 10_000;

/// Amount received for `amount_in` given reserves and a fee in bps.
/// UniswapV2 `getAmountOut`. `None` if inputs are degenerate.
#[must_use]
pub fn get_amount_out(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_bps: u64,
) -> Option<u64> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 || fee_bps >= FEE_DENOM {
        return None;
    }
    let amount_in_with_fee = u128::from(amount_in) * u128::from(FEE_DENOM - fee_bps);
    let numerator = amount_in_with_fee * u128::from(reserve_out);
    let denominator = u128::from(reserve_in) * u128::from(FEE_DENOM) + amount_in_with_fee;
    Some((numerator / denominator) as u64)
}

/// Input required to receive exactly `amount_out`. UniswapV2 `getAmountIn`.
/// `None` if the pool cannot deliver `amount_out`.
#[must_use]
pub fn get_amount_in(
    amount_out: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_bps: u64,
) -> Option<u64> {
    if amount_out == 0 || reserve_in == 0 || reserve_out <= amount_out || fee_bps >= FEE_DENOM {
        return None;
    }
    let numerator = u128::from(reserve_in) * u128::from(amount_out) * u128::from(FEE_DENOM);
    let denominator = u128::from(reserve_out - amount_out) * u128::from(FEE_DENOM - fee_bps);
    Some((numerator / denominator) as u64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_with_move_math() {
        // Mirrors arbitrage_system::amm_v2_tests::get_amount_out_matches_uniswap_v2
        assert_eq!(get_amount_out(1_000, 1_000, 1_000, 0), Some(500));
        assert_eq!(get_amount_out(1_000, 1_000, 1_000, 30), Some(499));
    }

    #[test]
    fn degenerate_inputs_return_none() {
        assert_eq!(get_amount_out(0, 1_000, 1_000, 30), None);
        assert_eq!(get_amount_out(1_000, 0, 1_000, 30), None);
        assert_eq!(get_amount_out(1_000, 1_000, 1_000, 10_000), None);
    }

    #[test]
    fn amount_in_round_trips() {
        let out = get_amount_out(1_000, 1_000_000, 1_000_000, 30).unwrap();
        let needed = get_amount_in(out, 1_000_000, 1_000_000, 30).unwrap();
        // get_amount_in rounds up, so it should never under-quote the input.
        assert!((999..=1_001).contains(&needed));
    }
}
