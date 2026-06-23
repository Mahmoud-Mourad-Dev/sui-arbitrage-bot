/// End-to-end tests for flash-loan arbitrage:
///   flash::borrow -> executor::begin -> swaps -> executor::settle_and_return
///   -> flash::repay -> transfer profit
/// exactly as the off-chain PTB builder assembles it. Borrowed capital, not owned.
#[test_only]
module arbitrage_system::flash_arb_tests;

use sui::coin::{Self, Coin};
use sui::test_scenario as ts;
use arbitrage_system::admin;
use arbitrage_system::amm_v2::{Self, Pool};
use arbitrage_system::amm_v2_adapter as adapter;
use arbitrage_system::executor;
use arbitrage_system::flash::{Self, FlashLender};
use arbitrage_system::test_coins::{A, B, C};

const TRADER: address = @0xA11CE;

// Pools priced so A->B->C->A is profitable (C ~ 2 A), 0.30% fee per hop.
fun seed(sc: &mut ts::Scenario, lender_fee_bps: u64) {
    let ctx = sc.ctx();
    let cap = admin::mint_for_testing(ctx);
    amm_v2::create_pool<A, B>(&cap, coin::mint_for_testing<A>(1_000, ctx), coin::mint_for_testing<B>(1_000, ctx), 30, ctx);
    amm_v2::create_pool<B, C>(&cap, coin::mint_for_testing<B>(1_000, ctx), coin::mint_for_testing<C>(1_000, ctx), 30, ctx);
    amm_v2::create_pool<C, A>(&cap, coin::mint_for_testing<C>(1_000, ctx), coin::mint_for_testing<A>(2_000, ctx), 30, ctx);
    // a flash vault holding A, seeded well beyond the loan size
    flash::create_lender<A>(&cap, coin::mint_for_testing<A>(1_000_000, ctx), lender_fee_bps, ctx);
    admin::burn_for_testing(cap);
}

#[test]
fun flash_arb_success_repays_and_profits() {
    let mut sc = ts::begin(TRADER);
    seed(&mut sc, 30);

    sc.next_tx(TRADER);
    {
        let mut pool_ab = ts::take_shared<Pool<A, B>>(&sc);
        let mut pool_bc = ts::take_shared<Pool<B, C>>(&sc);
        let mut pool_ca = ts::take_shared<Pool<C, A>>(&sc);
        let mut lender = ts::take_shared<FlashLender<A>>(&sc);
        let reserve_before = flash::reserve_value(&lender);
        let ctx = sc.ctx();

        // 1. borrow 10 A (fee = ceil(10 * 30/10000) = 1; due = 11)
        let (loan, frcpt) = flash::borrow(&mut lender, 10, ctx);
        assert!(flash::receipt_total(&frcpt) == 11, 0);

        // 2. open arb session; min_profit must cover the loan fee (1) + margin
        let (coin_a, arcpt) = executor::begin(loan, 2, ctx);

        // 3. A -> B -> C -> A
        let coin_b = adapter::swap_exact_in_a_to_b<A, B>(&mut pool_ab, coin_a, 0, ctx);
        let coin_c = adapter::swap_exact_in_a_to_b<B, C>(&mut pool_bc, coin_b, 0, ctx);
        let coin_a_back = adapter::swap_exact_in_a_to_b<C, A>(&mut pool_ca, coin_c, 0, ctx);

        // 4. profit gate (final >= 10 + min_profit) and keep the proceeds
        let proceeds = executor::settle_and_return(arcpt, coin_a_back);

        // 5. repay the loan (11) and keep the change as profit
        let profit = flash::repay(&mut lender, frcpt, proceeds, ctx);
        assert!(coin::value(&profit) > 0, 1);
        // vault made the fee back
        assert!(flash::reserve_value(&lender) == reserve_before + 1, 2);

        transfer::public_transfer(profit, TRADER);
        ts::return_shared(pool_ab);
        ts::return_shared(pool_bc);
        ts::return_shared(pool_ca);
        ts::return_shared(lender);
    };

    sc.next_tx(TRADER);
    {
        let profit = ts::take_from_sender<Coin<A>>(&sc);
        assert!(coin::value(&profit) > 0, 3); // ~4 A net of the loan + fee
        ts::return_to_sender(&sc, profit);
    };
    sc.end();
}

#[test]
#[expected_failure(abort_code = 1, location = arbitrage_system::flash)]
fun flash_repay_too_low_aborts() {
    let mut sc = ts::begin(TRADER);
    seed(&mut sc, 30);
    sc.next_tx(TRADER);
    {
        let mut lender = ts::take_shared<FlashLender<A>>(&sc);
        let ctx = sc.ctx();
        let (loan, frcpt) = flash::borrow(&mut lender, 1_000, ctx); // due = 1003
        coin::burn_for_testing(loan);
        // pay only 1000 < 1003 -> E_REPAY_TOO_LOW
        let change = flash::repay(&mut lender, frcpt, coin::mint_for_testing<A>(1_000, ctx), ctx);
        coin::burn_for_testing(change);
        ts::return_shared(lender);
    };
    sc.end();
}

#[test]
#[expected_failure(abort_code = 1, location = arbitrage_system::executor)]
fun flash_unprofitable_aborts_and_rolls_back() {
    let mut sc = ts::begin(TRADER);
    seed(&mut sc, 30);
    sc.next_tx(TRADER);
    {
        let mut lender = ts::take_shared<FlashLender<A>>(&sc);
        let ctx = sc.ctx();
        let (loan, frcpt) = flash::borrow(&mut lender, 1_000, ctx);
        // demand impossible profit -> settle_and_return aborts -> whole PTB reverts
        let (coin_a, arcpt) = executor::begin(loan, 1_000_000, ctx);
        let proceeds = executor::settle_and_return(arcpt, coin_a);
        let change = flash::repay(&mut lender, frcpt, proceeds, ctx);
        coin::burn_for_testing(change);
        ts::return_shared(lender);
    };
    sc.end();
}

#[test]
#[expected_failure(abort_code = 2, location = arbitrage_system::flash)]
fun flash_repay_wrong_lender_aborts() {
    let mut sc = ts::begin(TRADER);
    seed(&mut sc, 30);
    // a second, independent vault
    sc.next_tx(TRADER);
    {
        let ctx = sc.ctx();
        let cap = admin::mint_for_testing(ctx);
        flash::create_lender<A>(&cap, coin::mint_for_testing<A>(1_000_000, ctx), 30, ctx);
        admin::burn_for_testing(cap);
    };
    sc.next_tx(TRADER);
    {
        let mut lender1 = ts::take_shared<FlashLender<A>>(&sc);
        let mut lender2 = ts::take_shared<FlashLender<A>>(&sc);
        let ctx = sc.ctx();
        let (loan, frcpt) = flash::borrow(&mut lender1, 1_000, ctx);
        coin::burn_for_testing(loan);
        // repay the receipt to the WRONG vault -> E_WRONG_LENDER
        let change = flash::repay(&mut lender2, frcpt, coin::mint_for_testing<A>(2_000, ctx), ctx);
        coin::burn_for_testing(change);
        ts::return_shared(lender1);
        ts::return_shared(lender2);
    };
    sc.end();
}
