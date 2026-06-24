//! Concentrated-liquidity (Uniswap-V3-style) swap engine for Sui CLMMs
//! (Cetus, Turbos, Kriya-CLMM).
//!
//! WHY THIS EXISTS
//! ---------------
//! `amm.rs` models a single global `x*y=k` curve over total reserves. A CLMM has
//! liquidity `L` concentrated in tick ranges; within one range it behaves like
//! `x*y=k` on the *virtual* reserves `x = L·2^64/√P`, `y = L·√P/2^64`, but `L`
//! changes at every initialized tick. So pricing is a piecewise curve, not a
//! single hyperbola, and the pool's token *balances* are NOT the curve's reserves.
//!
//! FIXED POINT
//! -----------
//! Sui CLMMs store price as `sqrtPriceX64 = √(price) · 2^64` (Q64.64), where
//! `price = token1/token0`. (Ethereum/UniV3 uses X96; do not mix them.) All
//! products are done in `U256` to avoid overflow, then narrowed.
//!
//! CORRECTNESS
//! -----------
//! The functions below are the exact UniV3 `SqrtPriceMath`/`SwapMath` relations,
//! ported to X64. The test module proves: (1) a single infinite range with
//! `L=√(x·y)` reproduces `amm::get_amount_out` to within integer rounding — i.e.
//! this engine *generalizes* the testnet-validated V2 math; (2) monotonicity in
//! input, fee, and liquidity; (3) tick crossings add slippage. Bit-exact parity
//! with a specific venue (Cetus/Turbos) is closed separately by diffing against
//! that venue's on-chain quoter — see docs/external-venue-readiness-audit.md.
//!
//! NOTE: fee *growth* (feeGrowthGlobal/Outside) is LP accounting; it does NOT
//! affect swap output. Output depends only on the static fee rate (`fee_pips`).

use primitive_types::U256;
use serde::{Deserialize, Serialize};

/// √price in Q64.64.
pub type SqrtPriceX64 = u128;
/// Active liquidity (UniV3 `L`).
pub type Liquidity = u128;

/// 2^64 as the fixed-point unit.
pub const Q64: u128 = 1u128 << 64;
/// Fee denominator: fee is in pips (1e6 = 100%); 3000 pips = 0.30%.
pub const PIPS_DENOM: u128 = 1_000_000;

/// Engine sqrt-price bounds (production should use the venue's own MIN/MAX).
const MIN_SQRT_PRICE: u128 = 1u128 << 16;
const MAX_SQRT_PRICE: u128 = 1u128 << 120;

/// An initialized tick: its sqrt-price boundary and the net liquidity added when
/// crossing it left→right (price increasing), exactly as UniV3 `liquidityNet`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TickBoundary {
    pub sqrt_price: SqrtPriceX64,
    pub liquidity_net: i128,
}

/// Everything needed to quote a swap on one CLMM pool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClmmState {
    pub sqrt_price: SqrtPriceX64,
    pub liquidity: Liquidity,
    pub fee_pips: u64,
    /// Initialized tick boundaries, sorted ascending by `sqrt_price`.
    pub ticks: Vec<TickBoundary>,
}

// --- U256 helpers ---------------------------------------------------------

#[inline]
fn q64() -> U256 {
    U256::from(Q64)
}

#[inline]
fn u256_div_ceil(num: U256, den: U256) -> U256 {
    let q = num / den;
    if (num % den).is_zero() {
        q
    } else {
        q + U256::one()
    }
}

#[inline]
fn mul_div_floor(a: u128, b: u128, den: u128) -> u128 {
    (U256::from(a) * U256::from(b) / U256::from(den)).as_u128()
}

#[inline]
fn mul_div_ceil(a: u128, b: u128, den: u128) -> u128 {
    u256_div_ceil(U256::from(a) * U256::from(b), U256::from(den)).as_u128()
}

// --- amount <-> sqrtPrice deltas (UniV3 SqrtPriceMath, X64) ----------------

/// token0 amount between two sqrt prices for liquidity `l`:
/// `Δx = L·2^64·(hi-lo)/(hi·lo)`.
fn amount0_delta(sa: u128, sb: u128, l: u128, round_up: bool) -> u128 {
    let (lo, hi) = if sa < sb { (sa, sb) } else { (sb, sa) };
    if lo == 0 {
        return 0;
    }
    let numerator = U256::from(l) * q64() * U256::from(hi - lo);
    let denom = U256::from(hi) * U256::from(lo);
    let q = if round_up {
        u256_div_ceil(numerator, denom)
    } else {
        numerator / denom
    };
    q.as_u128()
}

/// token1 amount between two sqrt prices for liquidity `l`: `Δy = L·(hi-lo)/2^64`.
fn amount1_delta(sa: u128, sb: u128, l: u128, round_up: bool) -> u128 {
    let (lo, hi) = if sa < sb { (sa, sb) } else { (sb, sa) };
    let numerator = U256::from(l) * U256::from(hi - lo);
    let q = if round_up {
        u256_div_ceil(numerator, q64())
    } else {
        numerator / q64()
    };
    q.as_u128()
}

/// New sqrt price after adding `amount` of token0 (price decreases). Rounds up so
/// the resulting output is never overstated.
fn next_sqrt_from_amount0_in(sqrt_price: u128, l: u128, amount: u128) -> u128 {
    if amount == 0 {
        return sqrt_price;
    }
    let numerator = U256::from(l) * q64(); // L·2^64
    let product = U256::from(amount) * U256::from(sqrt_price);
    let denom = numerator + product;
    u256_div_ceil(numerator * U256::from(sqrt_price), denom).as_u128()
}

/// New sqrt price after adding `amount` of token1 (price increases). `√P + Δy·2^64/L`.
fn next_sqrt_from_amount1_in(sqrt_price: u128, l: u128, amount: u128) -> u128 {
    let add = (U256::from(amount) * q64()) / U256::from(l);
    (U256::from(sqrt_price) + add).as_u128()
}

/// One swap step within a single tick range (constant `L`), exact-in.
/// Returns `(sqrt_price_next, amount_in, amount_out, fee_amount)`.
fn compute_swap_step(
    sqrt_cur: u128,
    sqrt_target: u128,
    l: u128,
    amount_remaining: u128,
    fee_pips: u64,
    a_to_b: bool,
) -> (u128, u128, u128, u128) {
    let fee = fee_pips as u128;
    let amount_lf = mul_div_floor(amount_remaining, PIPS_DENOM - fee, PIPS_DENOM);

    let (sqrt_next, amount_in);
    if a_to_b {
        // token0 in, price down: target < cur
        let to_target = amount0_delta(sqrt_target, sqrt_cur, l, true);
        if amount_lf >= to_target {
            sqrt_next = sqrt_target;
            amount_in = to_target;
        } else {
            sqrt_next = next_sqrt_from_amount0_in(sqrt_cur, l, amount_lf);
            amount_in = amount0_delta(sqrt_next, sqrt_cur, l, true);
        }
    } else {
        // token1 in, price up: target > cur
        let to_target = amount1_delta(sqrt_cur, sqrt_target, l, true);
        if amount_lf >= to_target {
            sqrt_next = sqrt_target;
            amount_in = to_target;
        } else {
            sqrt_next = next_sqrt_from_amount1_in(sqrt_cur, l, amount_lf);
            amount_in = amount1_delta(sqrt_cur, sqrt_next, l, true);
        }
    }

    let amount_out = if a_to_b {
        amount1_delta(sqrt_next, sqrt_cur, l, false)
    } else {
        amount0_delta(sqrt_cur, sqrt_next, l, false)
    };

    let fee_amount = if sqrt_next != sqrt_target {
        // price capped by amount: all remaining beyond amount_in is fee
        amount_remaining - amount_in
    } else {
        mul_div_ceil(amount_in, fee, PIPS_DENOM - fee)
    };

    (sqrt_next, amount_in, amount_out, fee_amount)
}

/// Apply a tick's net liquidity when crossing it.
fn apply_net(l: u128, net: i128, a_to_b: bool) -> u128 {
    let delta = if a_to_b { -net } else { net };
    if delta >= 0 {
        l.saturating_add(delta as u128)
    } else {
        l.saturating_sub(delta.unsigned_abs())
    }
}

/// Quote an exact-input swap across tick ranges. `a_to_b = true` swaps token0→token1
/// (price decreasing). Returns the output amount, or `None` on degenerate input.
#[must_use]
pub fn quote_exact_in(state: &ClmmState, amount_in: u64, a_to_b: bool) -> Option<u64> {
    if amount_in == 0 || state.liquidity == 0 || state.fee_pips as u128 >= PIPS_DENOM {
        return None;
    }

    let mut sqrt_price = state.sqrt_price;
    let mut liquidity = state.liquidity;
    let mut remaining = amount_in as u128;
    let mut out: u128 = 0;

    // Boundaries we may cross, in travel order.
    let mut boundaries: Vec<TickBoundary> = if a_to_b {
        state
            .ticks
            .iter()
            .copied()
            .filter(|t| t.sqrt_price < sqrt_price)
            .collect()
    } else {
        state
            .ticks
            .iter()
            .copied()
            .filter(|t| t.sqrt_price > sqrt_price)
            .collect()
    };
    if a_to_b {
        boundaries.sort_by_key(|t| std::cmp::Reverse(t.sqrt_price)); // descending
    } else {
        boundaries.sort_by_key(|t| t.sqrt_price); // ascending
    }

    let mut iter = boundaries.into_iter();
    while remaining > 0 {
        let boundary = iter.next();
        let target = boundary.map(|t| t.sqrt_price).unwrap_or(if a_to_b {
            MIN_SQRT_PRICE
        } else {
            MAX_SQRT_PRICE
        });

        let (sqrt_next, amount_in_step, amount_out_step, fee) = compute_swap_step(
            sqrt_price,
            target,
            liquidity,
            remaining,
            state.fee_pips,
            a_to_b,
        );

        remaining = remaining.saturating_sub(amount_in_step + fee);
        out += amount_out_step;
        sqrt_price = sqrt_next;

        if sqrt_next == target {
            match boundary {
                Some(t) => {
                    liquidity = apply_net(liquidity, t.liquidity_net, a_to_b);
                    if liquidity == 0 {
                        break; // no liquidity beyond this tick
                    }
                }
                None => break, // hit the price-range limit
            }
        } else {
            break; // input exhausted inside the range
        }
    }

    u64::try_from(out).ok()
}

/// Convenience constructor for a pool with no initialized ticks in range (a single
/// active range) — used to model V2-equivalent depth and for quick quotes.
#[must_use]
pub fn single_range(sqrt_price: SqrtPriceX64, liquidity: Liquidity, fee_pips: u64) -> ClmmState {
    ClmmState {
        sqrt_price,
        liquidity,
        fee_pips,
        ticks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amm;

    /// Proof 1: a single infinite range with L=√(x·y) reproduces the V2 curve.
    /// We pick exact-square configs so √price and L are integers (no isqrt needed).
    #[test]
    fn single_range_reduces_to_v2() {
        // 1:1 pool, R each side -> sqrtP = 2^64, L = R.
        let r: u128 = 1_000_000_000_000;
        check_reduction(r, r, Q64, r, 1_000_000_000, 30);
        check_reduction(r, r, Q64, r, 50_000_000_000, 30);
        check_reduction(r, r, Q64, r, 1_000_000_000, 5);

        // 1:4 pool: reserve_a=R, reserve_b=4R -> price=4, sqrtP=2·2^64, L=√(R·4R)=2R.
        check_reduction(r, 4 * r, 2 * Q64, 2 * r, 1_000_000_000, 30);
        check_reduction(r, 4 * r, 2 * Q64, 2 * r, 25_000_000_000, 30);
    }

    fn check_reduction(
        reserve_a: u128,
        reserve_b: u128,
        sqrt_price: u128,
        liquidity: u128,
        amount: u64,
        fee_bps: u64,
    ) {
        let state = single_range(sqrt_price, liquidity, fee_bps * 100);
        // a_to_b = token0 (A) in -> output token1 (B)
        let clmm_out = quote_exact_in(&state, amount, true).unwrap();
        let v2_out =
            amm::get_amount_out(amount, reserve_a as u64, reserve_b as u64, fee_bps).unwrap();
        let diff = clmm_out.abs_diff(v2_out);
        // Identical up to fixed-point rounding (a few units on ~1e9-1e12 outputs).
        assert!(
            diff <= 8,
            "CLMM {clmm_out} vs V2 {v2_out} (diff {diff}) — should match within rounding"
        );
    }

    #[test]
    fn more_input_more_output() {
        let s = single_range(Q64, 1_000_000_000_000, 3000);
        let a = quote_exact_in(&s, 1_000_000_000, true).unwrap();
        let b = quote_exact_in(&s, 2_000_000_000, true).unwrap();
        assert!(b > a);
    }

    #[test]
    fn higher_fee_lower_output() {
        let lo = single_range(Q64, 1_000_000_000_000, 500); // 0.05%
        let hi = single_range(Q64, 1_000_000_000_000, 10_000); // 1.0%
        let out_lo = quote_exact_in(&lo, 1_000_000_000, true).unwrap();
        let out_hi = quote_exact_in(&hi, 1_000_000_000, true).unwrap();
        assert!(out_lo > out_hi);
    }

    #[test]
    fn deeper_liquidity_less_slippage() {
        let thin = single_range(Q64, 1_000_000_000_000, 3000);
        let deep = single_range(Q64, 1_000_000_000_000_000, 3000);
        let out_thin = quote_exact_in(&thin, 100_000_000_000, true).unwrap();
        let out_deep = quote_exact_in(&deep, 100_000_000_000, true).unwrap();
        // Deeper book -> output closer to input (less slippage), so strictly larger.
        assert!(out_deep > out_thin);
    }

    /// Proof 3: crossing a tick into thinner liquidity costs more than a hypothetical
    /// pool that stayed deep — i.e. tick structure changes the answer (V2 can't see this).
    #[test]
    fn tick_crossing_adds_slippage() {
        let l0: u128 = 1_000_000_000_000_000;
        // A downward (a_to_b) swap; just below current price, liquidity drops to 1/10.
        // liquidity_net is the change crossing UP, so to drop L when going DOWN we
        // set a positive net at that boundary (going down subtracts it).
        let boundary = TickBoundary {
            sqrt_price: Q64 - (Q64 / 1000), // slightly below current
            liquidity_net: (l0 - l0 / 10) as i128,
        };
        let with_cliff = ClmmState {
            sqrt_price: Q64,
            liquidity: l0,
            fee_pips: 3000,
            ticks: vec![boundary],
        };
        let no_cliff = single_range(Q64, l0, 3000);

        let amount = 50_000_000_000_000u64; // large enough to cross the boundary
        let out_cliff = quote_exact_in(&with_cliff, amount, true).unwrap();
        let out_flat = quote_exact_in(&no_cliff, amount, true).unwrap();
        // Same nominal current price/liquidity, but the liquidity cliff yields less.
        assert!(
            out_cliff < out_flat,
            "cliff {out_cliff} should be < flat {out_flat}"
        );
    }

    /// Quantifies the audit's core claim: using V2 on a pool's *balances* when the
    /// real depth is concentrated gives a very different (here, much worse) quote.
    #[test]
    fn v2_on_balances_misprices_concentrated_pool() {
        // Concentrated CLMM at 1:1 with deep L (0.01% fee).
        let clmm = single_range(Q64, 5_000_000_000_000_000, 100);
        // The pool's *token balances* are modest (typical for concentrated pools):
        // a naive V2 sim would use these as reserves.
        let bal_reserve: u64 = 200_000_000_000;
        let amount: u64 = 50_000_000_000;
        let clmm_out = quote_exact_in(&clmm, amount, true).unwrap();
        let v2_out = amm::get_amount_out(amount, bal_reserve, bal_reserve, 1).unwrap();
        // V2 sees a tiny book and predicts heavy slippage; CLMM is near 1:1.
        // The discrepancy is enormous relative to any micro-spread profit margin.
        assert!(clmm_out > v2_out);
        let gap = clmm_out - v2_out;
        assert!(gap > amount / 100, "expected >1% output gap, got {gap}");
    }
}
