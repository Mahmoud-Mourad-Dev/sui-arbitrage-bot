/// Turbos adapter (integration seam).
///
/// Same contract as every other adapter: normalize Turbos' swap entrypoint to the
/// exact-in `Coin<In>` -> exact-out `Coin<Out>` convention, enforce `min_out`, and
/// touch as few shared objects as possible.
///
/// HOW TO WIRE THIS UP
/// -------------------
/// 1. Add the Turbos interface package to `Move.toml` and pin it for your network.
/// 2. Replace the bodies with Turbos' swap call, e.g. its
///    `swap_router::swap_a_b` / `swap_b_a` (CLMM) returning the output coin, then
///    `assert!(coin::value(&out) >= min_out, E_SLIPPAGE)`.
///
/// Turbos pools carry a fee-tier type parameter (`Pool<A, B, FeeType>`); thread it
/// through the adapter signature when you wire it. The executor is unaffected — it
/// only sees `Coin<In>` in and `Coin<Out>` out.
module arbitrage_system::turbos_adapter;

use sui::coin::Coin;

/// Adapter has not been wired to the live Turbos package yet.
const E_ADAPTER_NOT_CONFIGURED: u64 = 1;

public fun swap_exact_in_a_to_b<A, B>(
    _coin_in: Coin<A>,
    _min_out: u64,
    _ctx: &mut TxContext,
): Coin<B> {
    abort E_ADAPTER_NOT_CONFIGURED
}

public fun swap_exact_in_b_to_a<A, B>(
    _coin_in: Coin<B>,
    _min_out: u64,
    _ctx: &mut TxContext,
): Coin<A> {
    abort E_ADAPTER_NOT_CONFIGURED
}
