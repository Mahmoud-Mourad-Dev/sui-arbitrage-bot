#[test_only]
module arbitrage_system::executor_tests;

use sui::coin::{Self, Coin};
use sui::test_scenario as ts;
use arbitrage_system::executor;
use arbitrage_system::test_coins::A;

const TRADER: address = @0xA11CE;

#[test]
fun settle_pays_out_principal_plus_profit() {
    let mut sc = ts::begin(TRADER);
    {
        let ctx = sc.ctx();
        let input = coin::mint_for_testing<A>(1_000, ctx);
        let (coin_in, receipt) = executor::begin(input, 10, ctx);
        // Simulate a profitable route: original input is consumed by the first
        // swap and 1_100 comes back at the end.
        coin::burn_for_testing(coin_in);
        let output = coin::mint_for_testing<A>(1_100, ctx);
        executor::settle(receipt, output);
    };
    sc.next_tx(TRADER);
    {
        let proceeds = ts::take_from_sender<Coin<A>>(&sc);
        assert!(coin::value(&proceeds) == 1_100, 0);
        ts::return_to_sender(&sc, proceeds);
    };
    sc.end();
}

#[test]
fun settle_passes_at_exact_target() {
    let mut sc = ts::begin(TRADER);
    {
        let ctx = sc.ctx();
        let (coin_in, receipt) = executor::begin(coin::mint_for_testing<A>(1_000, ctx), 50, ctx);
        coin::burn_for_testing(coin_in);
        // final == initial + min_profit exactly -> allowed
        executor::settle(receipt, coin::mint_for_testing<A>(1_050, ctx));
    };
    sc.next_tx(TRADER);
    {
        let proceeds = ts::take_from_sender<Coin<A>>(&sc);
        assert!(coin::value(&proceeds) == 1_050, 0);
        ts::return_to_sender(&sc, proceeds);
    };
    sc.end();
}

#[test]
#[expected_failure(abort_code = 1, location = arbitrage_system::executor)]
fun settle_aborts_when_below_target() {
    let mut sc = ts::begin(TRADER);
    {
        let ctx = sc.ctx();
        let (coin_in, receipt) = executor::begin(coin::mint_for_testing<A>(1_000, ctx), 10, ctx);
        coin::burn_for_testing(coin_in);
        // 1_005 < 1_000 + 10 -> must abort E_INSUFFICIENT_PROFIT (code 1)
        executor::settle(receipt, coin::mint_for_testing<A>(1_005, ctx));
    };
    sc.end();
}
