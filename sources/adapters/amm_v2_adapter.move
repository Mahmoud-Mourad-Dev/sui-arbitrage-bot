/// Adapter for the in-package reference AMM (`arbitrage_system::amm_v2`).
///
/// THE ADAPTER CONVENTION
/// ----------------------
/// Move has no interfaces/dynamic dispatch, so "adapters" are a *naming +
/// signature convention*, not a trait. Every adapter module MUST expose a pair of
/// functions shaped like this:
///
///   public fun swap_exact_in_a_to_b<A, B>(
///       pool: &mut <ConcretePool>, coin_in: Coin<A>, min_out: u64, ctx: &mut TxContext
///   ): Coin<B>
///   public fun swap_exact_in_b_to_a<A, B>( ... ): Coin<A>
///
/// Rules that keep the executor AMM-agnostic:
///   * Take an exact input `Coin<In>`, return an exact output `Coin<Out>`.
///   * Enforce `min_out` (per-hop slippage floor) and abort otherwise.
///   * Touch the minimum number of shared objects (ideally just the pool).
///   * Never reference `executor` — adapters know nothing about profit.
///
/// The off-chain PTB builder maps each route hop to one adapter call. Adding a new
/// venue = adding a new adapter module here. The executor never changes.
module arbitrage_system::amm_v2_adapter;

use sui::coin::Coin;
use arbitrage_system::amm_v2::{Self, Pool};

public fun swap_exact_in_a_to_b<A, B>(
    pool: &mut Pool<A, B>,
    coin_in: Coin<A>,
    min_out: u64,
    ctx: &mut TxContext,
): Coin<B> {
    amm_v2::swap_a_to_b(pool, coin_in, min_out, ctx)
}

public fun swap_exact_in_b_to_a<A, B>(
    pool: &mut Pool<A, B>,
    coin_in: Coin<B>,
    min_out: u64,
    ctx: &mut TxContext,
): Coin<A> {
    amm_v2::swap_b_to_a(pool, coin_in, min_out, ctx)
}
