/// Module: scaffold
///
/// A minimal starting point for a Sui Move package. Replace the contents of this
/// module with your own objects, functions, and entry points.
module sui_arbitrage_bot::scaffold;

use std::string::{Self, String};

/// A simple owned object you can mint, read, and transfer.
/// Delete or replace this with your own types.
public struct Greeting has key, store {
    id: UID,
    message: String,
}

/// Create a new `Greeting` object and transfer it to the caller.
public fun mint(message: vector<u8>, ctx: &mut TxContext) {
    let greeting = Greeting {
        id: object::new(ctx),
        message: string::utf8(message),
    };
    transfer::public_transfer(greeting, ctx.sender());
}

/// Read the message stored in a `Greeting`.
public fun message(greeting: &Greeting): String {
    greeting.message
}
