/// End-to-end test of the executor + adapter pattern over a 3-hop cycle
/// A -> B -> C -> A, exactly as the off-chain engine would assemble it in a PTB.
#[test_only]
module arbitrage_system::arb_route_tests;

use sui::coin::{Self, Coin};
use sui::test_scenario as ts;
use arbitrage_system::admin;
use arbitrage_system::amm_v2::{Self, Pool};
use arbitrage_system::amm_v2_adapter as adapter;
use arbitrage_system::executor;
use arbitrage_system::test_coins::{A, B, C};

const TRADER: address = @0xA11CE;

// Pools are priced so a round trip is profitable:
//   AB: 1000 A / 1000 B   (1:1)
//   BC: 1000 B / 1000 C   (1:1)
//   CA: 1000 C / 2000 A   (C is worth ~2 A) <- the dislocation
// Starting with 10 A and 0.30% fee per hop, ~15 A comes back.
fun seed_pools(sc: &mut ts::Scenario) {
    let ctx = sc.ctx();
    let cap = admin::mint_for_testing(ctx);
    amm_v2::create_pool<A, B>(
        &cap,
        coin::mint_for_testing<A>(1_000, ctx),
        coin::mint_for_testing<B>(1_000, ctx),
        30,
        ctx,
    );
    amm_v2::create_pool<B, C>(
        &cap,
        coin::mint_for_testing<B>(1_000, ctx),
        coin::mint_for_testing<C>(1_000, ctx),
        30,
        ctx,
    );
    amm_v2::create_pool<C, A>(
        &cap,
        coin::mint_for_testing<C>(1_000, ctx),
        coin::mint_for_testing<A>(2_000, ctx),
        30,
        ctx,
    );
    admin::burn_for_testing(cap);
}

#[test]
fun atomic_three_hop_cycle_is_profitable() {
    let mut sc = ts::begin(TRADER);
    seed_pools(&mut sc);

    sc.next_tx(TRADER);
    {
        let mut pool_ab = ts::take_shared<Pool<A, B>>(&sc);
        let mut pool_bc = ts::take_shared<Pool<B, C>>(&sc);
        let mut pool_ca = ts::take_shared<Pool<C, A>>(&sc);
        let ctx = sc.ctx();

        // 1. open session
        let input = coin::mint_for_testing<A>(10, ctx);
        let (coin_a, receipt) = executor::begin(input, 1, ctx);

        // 2-4. swaps through the adapter (executor never sees the pools)
        let coin_b = adapter::swap_exact_in_a_to_b<A, B>(&mut pool_ab, coin_a, 0, ctx);
        let coin_c = adapter::swap_exact_in_a_to_b<B, C>(&mut pool_bc, coin_b, 0, ctx);
        let coin_a_back = adapter::swap_exact_in_a_to_b<C, A>(&mut pool_ca, coin_c, 0, ctx);

        // 5. profit gate (final must clear initial + min_profit)
        executor::settle(receipt, coin_a_back);

        ts::return_shared(pool_ab);
        ts::return_shared(pool_bc);
        ts::return_shared(pool_ca);
    };

    sc.next_tx(TRADER);
    {
        let proceeds = ts::take_from_sender<Coin<A>>(&sc);
        // started with 10, must come back with strictly more
        assert!(coin::value(&proceeds) > 10, 0);
        ts::return_to_sender(&sc, proceeds);
    };
    sc.end();
}

#[test]
#[expected_failure(abort_code = 1, location = arbitrage_system::executor)]
fun unprofitable_cycle_aborts_atomically() {
    let mut sc = ts::begin(TRADER);
    seed_pools(&mut sc);

    sc.next_tx(TRADER);
    {
        let mut pool_ab = ts::take_shared<Pool<A, B>>(&sc);
        let mut pool_bc = ts::take_shared<Pool<B, C>>(&sc);
        let mut pool_ca = ts::take_shared<Pool<C, A>>(&sc);
        let ctx = sc.ctx();

        // Demand an unrealistic profit -> settle aborts -> whole PTB reverts.
        let input = coin::mint_for_testing<A>(10, ctx);
        let (coin_a, receipt) = executor::begin(input, 1_000_000, ctx);
        let coin_b = adapter::swap_exact_in_a_to_b<A, B>(&mut pool_ab, coin_a, 0, ctx);
        let coin_c = adapter::swap_exact_in_a_to_b<B, C>(&mut pool_bc, coin_b, 0, ctx);
        let coin_a_back = adapter::swap_exact_in_a_to_b<C, A>(&mut pool_ca, coin_c, 0, ctx);
        executor::settle(receipt, coin_a_back);

        ts::return_shared(pool_ab);
        ts::return_shared(pool_bc);
        ts::return_shared(pool_ca);
    };
    sc.end();
}
