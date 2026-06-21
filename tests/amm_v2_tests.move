#[test_only]
module arbitrage_system::amm_v2_tests;

use sui::coin::{Self, Coin};
use sui::test_scenario as ts;
use arbitrage_system::admin;
use arbitrage_system::amm_v2::{Self, Pool};
use arbitrage_system::math;
use arbitrage_system::test_coins::{A, B};

const LP: address = @0xB0B;
const TRADER: address = @0xA11CE;

#[test]
fun get_amount_out_matches_uniswap_v2() {
    // No fee: x*y=k. in=1000 into 1000/1000 -> 1000*1000/2000 = 500.
    assert!(math::get_amount_out(1_000, 1_000, 1_000, 0) == 500, 0);
    // 0.30% fee: 9_970_000 * 1000 / (10_000_000 + 9_970_000) = 499.
    assert!(math::get_amount_out(1_000, 1_000, 1_000, 30) == 499, 1);
}

#[test]
fun swap_moves_reserves_and_returns_output() {
    let mut sc = ts::begin(LP);
    {
        let ctx = sc.ctx();
        let cap = admin::mint_for_testing(ctx);
        amm_v2::create_pool<A, B>(
            &cap,
            coin::mint_for_testing<A>(1_000_000, ctx),
            coin::mint_for_testing<B>(1_000_000, ctx),
            30,
            ctx,
        );
        admin::burn_for_testing(cap);
    };

    sc.next_tx(TRADER);
    {
        let mut pool = ts::take_shared<Pool<A, B>>(&sc);
        let (ra0, rb0) = amm_v2::reserves(&pool);
        let expected = amm_v2::quote_a_to_b(&pool, 1_000);
        let ctx = sc.ctx();
        let out = amm_v2::swap_a_to_b(&mut pool, coin::mint_for_testing<A>(1_000, ctx), 0, ctx);
        assert!(coin::value(&out) == expected, 0);
        let (ra1, rb1) = amm_v2::reserves(&pool);
        assert!(ra1 == ra0 + 1_000, 1);
        assert!(rb1 == rb0 - expected, 2);
        coin::burn_for_testing(out);
        ts::return_shared(pool);
    };
    sc.end();
}

#[test]
#[expected_failure(abort_code = 1, location = arbitrage_system::amm_v2)]
fun swap_aborts_on_slippage() {
    let mut sc = ts::begin(LP);
    {
        let ctx = sc.ctx();
        let cap = admin::mint_for_testing(ctx);
        amm_v2::create_pool<A, B>(
            &cap,
            coin::mint_for_testing<A>(1_000_000, ctx),
            coin::mint_for_testing<B>(1_000_000, ctx),
            30,
            ctx,
        );
        admin::burn_for_testing(cap);
    };
    sc.next_tx(TRADER);
    {
        let mut pool = ts::take_shared<Pool<A, B>>(&sc);
        let ctx = sc.ctx();
        // demand more out than possible -> E_SLIPPAGE (code 1)
        let out: Coin<B> = amm_v2::swap_a_to_b(
            &mut pool,
            coin::mint_for_testing<A>(1_000, ctx),
            10_000_000,
            ctx,
        );
        coin::burn_for_testing(out);
        ts::return_shared(pool);
    };
    sc.end();
}
