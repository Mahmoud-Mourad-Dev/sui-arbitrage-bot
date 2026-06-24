/// Cetus CLMM adapter.
///
/// Normalizes Cetus's flash-swap entrypoint to the executor's adapter convention
/// (exact-in `Coin<In>` -> exact-out `Coin<Out>`), so the PTB builder treats a Cetus
/// hop exactly like the in-package `amm_v2_adapter`.
///
/// Cetus has no plain exact-in `swap`; the public path is `flash_swap` +
/// `repay_flash_swap`: `flash_swap` hands you the output balance and a hot-potato
/// receipt recording what you owe, then `repay_flash_swap` consumes the receipt once
/// you hand back the input. We run both in one call so the adapter is a normal
/// exact-in swap from the executor's point of view. Slippage is enforced here
/// (`min_out`) and again by the executor's profit gate downstream.
module arbitrage_system::cetus_adapter;

use sui::balance;
use sui::clock::Clock;
use sui::coin::{Self, Coin};
use cetusclmm::config::GlobalConfig;
use cetusclmm::pool::{Self, Pool};
use cetusclmm::tick_math;

/// Output fell below `min_out`.
const E_SLIPPAGE: u64 = 1;

/// Swap exact `coin_in` of A for B (a2b: price decreasing). Aborts if out < `min_out`.
public fun swap_exact_in_a_to_b<A, B>(
    config: &GlobalConfig,
    pool: &mut Pool<A, B>,
    coin_in: Coin<A>,
    min_out: u64,
    clock: &Clock,
    ctx: &mut TxContext,
): Coin<B> {
    let amount_in = coin::value(&coin_in);
    // by_amount_in = true: exact-in. min sqrt-price = no artificial price cap; the
    // economic guard is `min_out` + the executor gate, not the price limit.
    let (recv_a, recv_b, receipt) = pool::flash_swap<A, B>(
        config,
        pool,
        true, // a2b
        true, // by_amount_in
        amount_in,
        tick_math::min_sqrt_price(),
        clock,
    );
    // a2b yields B; nothing is received on the A side.
    balance::destroy_zero(recv_a);

    // Pay exactly what the receipt records (== amount_in for exact-in) and settle.
    let pay = pool::swap_pay_amount<A, B>(&receipt);
    let mut in_bal = coin::into_balance(coin_in);
    let pay_bal = balance::split(&mut in_bal, pay);
    pool::repay_flash_swap<A, B>(config, pool, pay_bal, balance::zero<B>(), receipt);
    balance::destroy_zero(in_bal); // exact-in: pay == amount_in, so no dust remains

    let out = coin::from_balance(recv_b, ctx);
    assert!(coin::value(&out) >= min_out, E_SLIPPAGE);
    out
}

/// Swap exact `coin_in` of B for A (b2a: price increasing). Aborts if out < `min_out`.
public fun swap_exact_in_b_to_a<A, B>(
    config: &GlobalConfig,
    pool: &mut Pool<A, B>,
    coin_in: Coin<B>,
    min_out: u64,
    clock: &Clock,
    ctx: &mut TxContext,
): Coin<A> {
    let amount_in = coin::value(&coin_in);
    let (recv_a, recv_b, receipt) = pool::flash_swap<A, B>(
        config,
        pool,
        false, // b2a
        true, // by_amount_in
        amount_in,
        tick_math::max_sqrt_price(),
        clock,
    );
    balance::destroy_zero(recv_b);

    let pay = pool::swap_pay_amount<A, B>(&receipt);
    let mut in_bal = coin::into_balance(coin_in);
    let pay_bal = balance::split(&mut in_bal, pay);
    pool::repay_flash_swap<A, B>(config, pool, balance::zero<A>(), pay_bal, receipt);
    balance::destroy_zero(in_bal);

    let out = coin::from_balance(recv_a, ctx);
    assert!(coin::value(&out) >= min_out, E_SLIPPAGE);
    out
}
