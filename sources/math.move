/// Constant-product (x*y=k) swap math, shared by the in-package reference AMM
/// and any adapter that wants Uniswap-V2-style quoting. Pure functions only —
/// no objects, no state. Kept identical to the off-chain Rust implementation in
/// `offchain/src/amm.rs` so simulation matches on-chain execution exactly.
module arbitrage_system::math;

/// Fee is expressed in basis points of 1/10_000 (e.g. 30 = 0.30%).
const FEE_DENOM: u64 = 10_000;

const E_ZERO_INPUT: u64 = 100;
const E_INSUFFICIENT_LIQUIDITY: u64 = 101;
const E_BAD_FEE: u64 = 102;

public fun fee_denom(): u64 { FEE_DENOM }

/// Amount received for `amount_in`, given reserves and a fee in bps.
/// Mirrors UniswapV2's getAmountOut. All intermediate math is u128 to avoid
/// overflow; inputs/outputs are u64 (Sui coin amounts).
public fun get_amount_out(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_bps: u64,
): u64 {
    assert!(amount_in > 0, E_ZERO_INPUT);
    assert!(reserve_in > 0 && reserve_out > 0, E_INSUFFICIENT_LIQUIDITY);
    assert!(fee_bps < FEE_DENOM, E_BAD_FEE);

    let amount_in_with_fee = (amount_in as u128) * ((FEE_DENOM - fee_bps) as u128);
    let numerator = amount_in_with_fee * (reserve_out as u128);
    let denominator = (reserve_in as u128) * (FEE_DENOM as u128) + amount_in_with_fee;
    (numerator / denominator) as u64
}

/// Amount of input required to receive exactly `amount_out` (UniswapV2 getAmountIn).
/// Useful off-chain for sizing trades; included on-chain for completeness.
public fun get_amount_in(
    amount_out: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_bps: u64,
): u64 {
    assert!(amount_out > 0, E_ZERO_INPUT);
    assert!(reserve_in > 0 && reserve_out > amount_out, E_INSUFFICIENT_LIQUIDITY);
    assert!(fee_bps < FEE_DENOM, E_BAD_FEE);

    let numerator = (reserve_in as u128) * (amount_out as u128) * (FEE_DENOM as u128);
    let denominator = ((reserve_out - amount_out) as u128) * ((FEE_DENOM - fee_bps) as u128);
    ((numerator / denominator) as u64) + 1
}
