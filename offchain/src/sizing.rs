//! Trade-size optimization.
//!
//! Arbitrage net profit as a function of input size is **unimodal**: it rises as the
//! captured spread grows, peaks, then falls as price impact (and, for borrowed capital,
//! the flash fee) overtakes the edge. A fixed grid of candidate sizes either misses the
//! peak (too coarse) or wastes work (too fine), and — critically — a grid whose values
//! are all tiny makes fixed gas dominate so the profit gate can never clear.
//!
//! This module ternary-searches the peak over an `[lo, hi]` range in O(log(hi−lo))
//! evaluations. The objective is supplied by the caller as a closure returning **signed**
//! net (so the search can compare losing sizes too) and `None` for an infeasible size
//! (e.g. an input that exhausts a pool's quotable liquidity). Infeasibility is treated as
//! a prefix property (if size `x` can't be quoted, neither can anything larger), which
//! holds for constant-product and single-range CLMM math; the search first clamps `hi`
//! down to the largest feasible size, then ternary-searches the feasible prefix.

/// Argmax of a unimodal `f` over the integer range `[lo, hi]`, returning
/// `(best_input, best_value)`. `f` returns `None` for infeasible inputs (assumed to be a
/// suffix of the range). Returns `None` only if no input in the range is feasible.
pub fn ternary_max<F>(f: &F, lo: u64, hi: u64) -> Option<(u64, i128)>
where
    F: Fn(u64) -> Option<i128>,
{
    if lo > hi {
        return None;
    }
    // Clamp `hi` to the largest feasible input (feasibility is a prefix property).
    let hi = feasible_upper_bound(f, lo, hi)?;

    let (mut lo, mut hi) = (lo, hi);
    // Narrow to a tiny window with integer ternary search, then scan it exactly.
    while hi - lo > 2 {
        let third = (hi - lo) / 3;
        let m1 = lo + third;
        let m2 = hi - third;
        let v1 = f(m1).unwrap_or(i128::MIN);
        let v2 = f(m2).unwrap_or(i128::MIN);
        if v1 < v2 {
            lo = m1 + 1;
        } else {
            hi = m2 - 1;
        }
    }

    // Exact max over the remaining ≤3 points.
    let mut best: Option<(u64, i128)> = None;
    let mut x = lo;
    while x <= hi {
        if let Some(v) = f(x) {
            if best.is_none_or(|(_, bv)| v > bv) {
                best = Some((x, v));
            }
        }
        x += 1;
    }
    best
}

/// Largest input in `[lo, hi]` for which `f` is feasible (`Some`), or `None` if even `lo`
/// is infeasible. Assumes feasibility is a prefix of the range.
fn feasible_upper_bound<F>(f: &F, lo: u64, hi: u64) -> Option<u64>
where
    F: Fn(u64) -> Option<i128>,
{
    if f(hi).is_some() {
        return Some(hi);
    }
    f(lo)?; // `lo` must be feasible, else nothing in the range is.
            // Binary-search the boundary: `lo` feasible, `hi` infeasible.
    let (mut lo, mut hi) = (lo, hi);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if f(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_peak_of_concave_parabola() {
        // f(x) = -(x-70)^2 + 1000, peak at x=70.
        let f = |x: u64| Some(1000 - (x as i128 - 70).pow(2));
        let (arg, val) = ternary_max(&f, 0, 200).unwrap();
        assert_eq!(arg, 70);
        assert_eq!(val, 1000);
    }

    #[test]
    fn respects_infeasible_suffix() {
        // Peak would be at 120, but anything > 90 is infeasible → best feasible is 90.
        let f = |x: u64| {
            if x > 90 {
                None
            } else {
                Some(5000 - (x as i128 - 120).pow(2))
            }
        };
        let (arg, _) = ternary_max(&f, 0, 200).unwrap();
        assert_eq!(arg, 90);
    }

    #[test]
    fn monotonic_increasing_picks_top() {
        let f = |x: u64| Some(x as i128);
        let (arg, val) = ternary_max(&f, 10, 50).unwrap();
        assert_eq!(arg, 50);
        assert_eq!(val, 50);
    }

    #[test]
    fn degenerate_single_point() {
        let f = |x: u64| Some(x as i128 * 2);
        assert_eq!(ternary_max(&f, 42, 42), Some((42, 84)));
    }

    #[test]
    fn all_infeasible_is_none() {
        let f = |_x: u64| None::<i128>;
        assert_eq!(ternary_max(&f, 0, 100), None);
    }

    #[test]
    fn peak_at_left_edge() {
        let f = |x: u64| Some(-(x as i128));
        let (arg, _) = ternary_max(&f, 5, 99).unwrap();
        assert_eq!(arg, 5);
    }
}
