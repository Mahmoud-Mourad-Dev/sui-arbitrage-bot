/// Testnet validation token B. Mintable via the shared TreasuryCap.
module validation_coins::coin_b;

use sui::coin::{Self, Coin, TreasuryCap};

public struct COIN_B has drop {}

fun init(witness: COIN_B, ctx: &mut TxContext) {
    let (treasury, metadata) = coin::create_currency(
        witness,
        9,
        b"VALB",
        b"Validation Coin B",
        b"Testnet validation token B",
        option::none(),
        ctx,
    );
    transfer::public_freeze_object(metadata);
    transfer::public_transfer(treasury, ctx.sender());
}

/// Mint `amount` and return the coin (composable inside a PTB).
public fun mint(treasury: &mut TreasuryCap<COIN_B>, amount: u64, ctx: &mut TxContext): Coin<COIN_B> {
    coin::mint(treasury, amount, ctx)
}
