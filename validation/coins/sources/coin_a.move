/// Testnet validation token A. Mintable via the shared TreasuryCap.
module validation_coins::coin_a;

use sui::coin::{Self, Coin, TreasuryCap};

public struct COIN_A has drop {}

fun init(witness: COIN_A, ctx: &mut TxContext) {
    let (treasury, metadata) = coin::create_currency(
        witness,
        9,
        b"VALA",
        b"Validation Coin A",
        b"Testnet validation token A",
        option::none(),
        ctx,
    );
    transfer::public_freeze_object(metadata);
    transfer::public_transfer(treasury, ctx.sender());
}

/// Mint `amount` and return the coin (composable inside a PTB).
public fun mint(treasury: &mut TreasuryCap<COIN_A>, amount: u64, ctx: &mut TxContext): Coin<COIN_A> {
    coin::mint(treasury, amount, ctx)
}
