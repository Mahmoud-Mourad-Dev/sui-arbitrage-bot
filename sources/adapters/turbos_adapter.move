/// Turbos CLMM adapter.
///
/// Normalizes Turbos's `swap_router` to the executor's adapter convention (exact-in
/// `Coin<In>` -> exact-out `Coin<Out>`). Turbos pools are typed by a third `FeeType`
/// fee-tier parameter, so these functions carry `<A, B, FeeType>` (vs `<A, B>` for
/// the constant-product / Cetus adapters); the PTB builder supplies the fee-tier type
/// argument per hop.
///
/// `swap_*_with_return_` returns the output coin plus any unspent input change. For
/// exact-in the change is zero; we route any dust back to the sender defensively.
/// Slippage is enforced via `amount_threshold` (= `min_out`) and re-checked here, then
/// again by the executor's profit gate.
module arbitrage_system::turbos_adapter;

use sui::clock::{Self, Clock};
use sui::coin::{Self, Coin};
use turbos_clmm::pool::{Pool, Versioned};
use turbos_clmm::swap_router;

/// Output fell below `min_out`.
const E_SLIPPAGE: u64 = 1;

/// CLMM sqrt-price bounds (Q64.64). Used as permissive price limits — the economic
/// guard is `min_out` + the executor gate, not the price cap.
const MIN_SQRT_PRICE_X64: u128 = 4295048016;
const MAX_SQRT_PRICE_X64: u128 = 79226673515401279992447579055;

/// How far ahead of the current block time to set the swap deadline (ms).
const DEADLINE_BUFFER_MS: u64 = 3_600_000;

/// Swap exact `coin_in` of A for B (a2b). Aborts if out < `min_out`.
public fun swap_exact_in_a_to_b<A, B, FeeType>(
    pool: &mut Pool<A, B, FeeType>,
    coin_in: Coin<A>,
    min_out: u64,
    clock: &Clock,
    versioned: &Versioned,
    ctx: &mut TxContext,
): Coin<B> {
    let amount_in = coin::value(&coin_in);
    let recipient = ctx.sender();
    let deadline = clock::timestamp_ms(clock) + DEADLINE_BUFFER_MS;
    let (out_b, change_a) = swap_router::swap_a_b_with_return_<A, B, FeeType>(
        pool,
        vector[coin_in],
        amount_in,
        min_out, // amount_threshold
        MIN_SQRT_PRICE_X64 + 1,
        true, // is_exact_in
        recipient,
        deadline,
        clock,
        versioned,
        ctx,
    );
    return_change(change_a, recipient);
    assert!(coin::value(&out_b) >= min_out, E_SLIPPAGE);
    out_b
}

/// Swap exact `coin_in` of B for A (b2a). Aborts if out < `min_out`.
public fun swap_exact_in_b_to_a<A, B, FeeType>(
    pool: &mut Pool<A, B, FeeType>,
    coin_in: Coin<B>,
    min_out: u64,
    clock: &Clock,
    versioned: &Versioned,
    ctx: &mut TxContext,
): Coin<A> {
    let amount_in = coin::value(&coin_in);
    let recipient = ctx.sender();
    let deadline = clock::timestamp_ms(clock) + DEADLINE_BUFFER_MS;
    let (out_a, change_b) = swap_router::swap_b_a_with_return_<A, B, FeeType>(
        pool,
        vector[coin_in],
        amount_in,
        min_out,
        MAX_SQRT_PRICE_X64 - 1,
        true,
        recipient,
        deadline,
        clock,
        versioned,
        ctx,
    );
    return_change(change_b, recipient);
    assert!(coin::value(&out_a) >= min_out, E_SLIPPAGE);
    out_a
}

/// Send unspent input change back to the trader (zero for exact-in, but defensive).
fun return_change<T>(change: Coin<T>, recipient: address) {
    if (coin::value(&change) == 0) {
        coin::destroy_zero(change);
    } else {
        transfer::public_transfer(change, recipient);
    }
}
