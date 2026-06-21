/// Reference Uniswap-V2-style constant-product AMM.
///
/// Purpose: (1) a concrete, gas-honest pool the executor + adapter pattern can be
/// tested against end-to-end, and (2) the canonical shape for the "future
/// Uniswap-V2 style pools on Sui" the system is meant to route through.
///
/// Pools are shared objects (unavoidable for an AMM — anyone must be able to
/// swap). Liquidity is admin-managed here to keep the surface minimal and avoid
/// LP-token bookkeeping that the arbitrage system does not need. Swaps are
/// permissionless. Each swap touches exactly one shared object and creates one
/// output coin — nothing else.
module arbitrage_system::amm_v2;

use sui::balance::{Self, Balance};
use sui::coin::{Self, Coin};
use sui::event;
use arbitrage_system::admin::AdminCap;
use arbitrage_system::math;

const E_SLIPPAGE: u64 = 1;

/// `phantom` type params: the pool stores balances, not the witness types.
public struct Pool<phantom A, phantom B> has key {
    id: UID,
    reserve_a: Balance<A>,
    reserve_b: Balance<B>,
    fee_bps: u64,
}

public struct PoolCreated has copy, drop {
    pool: ID,
    fee_bps: u64,
    reserve_a: u64,
    reserve_b: u64,
}

public struct Swapped has copy, drop {
    pool: ID,
    amount_in: u64,
    amount_out: u64,
    a_to_b: bool,
}

/// Create and share a pool seeded with initial liquidity. Admin-gated.
public fun create_pool<A, B>(
    _admin: &AdminCap,
    coin_a: Coin<A>,
    coin_b: Coin<B>,
    fee_bps: u64,
    ctx: &mut TxContext,
) {
    assert!(fee_bps < math::fee_denom(), E_SLIPPAGE);
    let reserve_a = coin::into_balance(coin_a);
    let reserve_b = coin::into_balance(coin_b);
    let pool = Pool<A, B> {
        id: object::new(ctx),
        fee_bps,
        reserve_a,
        reserve_b,
    };
    event::emit(PoolCreated {
        pool: object::id(&pool),
        fee_bps,
        reserve_a: balance::value(&pool.reserve_a),
        reserve_b: balance::value(&pool.reserve_b),
    });
    transfer::share_object(pool);
}

/// Top up reserves. Admin-gated.
public fun fund<A, B>(
    _admin: &AdminCap,
    pool: &mut Pool<A, B>,
    coin_a: Coin<A>,
    coin_b: Coin<B>,
) {
    balance::join(&mut pool.reserve_a, coin::into_balance(coin_a));
    balance::join(&mut pool.reserve_b, coin::into_balance(coin_b));
}

/// Swap exact `coin_in` of A for B. Aborts if output < `min_out`.
public fun swap_a_to_b<A, B>(
    pool: &mut Pool<A, B>,
    coin_in: Coin<A>,
    min_out: u64,
    ctx: &mut TxContext,
): Coin<B> {
    let amount_in = coin::value(&coin_in);
    let amount_out = math::get_amount_out(
        amount_in,
        balance::value(&pool.reserve_a),
        balance::value(&pool.reserve_b),
        pool.fee_bps,
    );
    assert!(amount_out >= min_out, E_SLIPPAGE);
    balance::join(&mut pool.reserve_a, coin::into_balance(coin_in));
    let out = coin::take(&mut pool.reserve_b, amount_out, ctx);
    event::emit(Swapped { pool: object::id(pool), amount_in, amount_out, a_to_b: true });
    out
}

/// Swap exact `coin_in` of B for A. Aborts if output < `min_out`.
public fun swap_b_to_a<A, B>(
    pool: &mut Pool<A, B>,
    coin_in: Coin<B>,
    min_out: u64,
    ctx: &mut TxContext,
): Coin<A> {
    let amount_in = coin::value(&coin_in);
    let amount_out = math::get_amount_out(
        amount_in,
        balance::value(&pool.reserve_b),
        balance::value(&pool.reserve_a),
        pool.fee_bps,
    );
    assert!(amount_out >= min_out, E_SLIPPAGE);
    balance::join(&mut pool.reserve_b, coin::into_balance(coin_in));
    let out = coin::take(&mut pool.reserve_a, amount_out, ctx);
    event::emit(Swapped { pool: object::id(pool), amount_in, amount_out, a_to_b: false });
    out
}

// --- views (used off-chain via dev-inspect and in tests) ---

public fun reserves<A, B>(pool: &Pool<A, B>): (u64, u64) {
    (balance::value(&pool.reserve_a), balance::value(&pool.reserve_b))
}

public fun fee_bps<A, B>(pool: &Pool<A, B>): u64 { pool.fee_bps }

public fun quote_a_to_b<A, B>(pool: &Pool<A, B>, amount_in: u64): u64 {
    math::get_amount_out(
        amount_in,
        balance::value(&pool.reserve_a),
        balance::value(&pool.reserve_b),
        pool.fee_bps,
    )
}

public fun quote_b_to_a<A, B>(pool: &Pool<A, B>, amount_in: u64): u64 {
    math::get_amount_out(
        amount_in,
        balance::value(&pool.reserve_b),
        balance::value(&pool.reserve_a),
        pool.fee_bps,
    )
}
