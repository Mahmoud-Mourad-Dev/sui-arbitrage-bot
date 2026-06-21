/// Testnet validation token C. Mintable via the shared TreasuryCap.
module validation_coins::coin_c;

use sui::coin::{Self, Coin, TreasuryCap};

public struct COIN_C has drop {}

fun init(witness: COIN_C, ctx: &mut TxContext) {
    let (treasury, metadata) = coin::create_currency(
        witness,
        9,
        b"VALC",
        b"Validation Coin C",
        b"Testnet validation token C",
        option::none(),
        ctx,
    );
    transfer::public_freeze_object(metadata);
    transfer::public_transfer(treasury, ctx.sender());
}

/// Mint `amount` and return the coin (composable inside a PTB).
public fun mint(treasury: &mut TreasuryCap<COIN_C>, amount: u64, ctx: &mut TxContext): Coin<COIN_C> {
    coin::mint(treasury, amount, ctx)
}
