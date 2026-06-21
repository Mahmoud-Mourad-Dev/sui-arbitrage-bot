/// Atomic arbitrage executor — the only on-chain guarantee the system needs.
///
/// Design
/// ------
/// The executor is **stateless**: no shared objects, no global mutable state, no
/// AMM knowledge. It creates exactly one short-lived value (a hot-potato receipt)
/// and zero persistent objects. Routing happens entirely off-chain and is
/// assembled as a Programmable Transaction Block (PTB):
///
///   1. (coin, receipt) = executor::begin(input_coin, min_profit)
///   2. coin_b = <adapter>::swap(pool_ab, coin,   min_out_b, ...)   // A -> B
///   3. coin_c = <adapter>::swap(pool_bc, coin_b, min_out_c, ...)   // B -> C
///   4. coin_a = <adapter>::swap(pool_ca, coin_c, min_out_a, ...)   // C -> A
///   5. executor::settle(receipt, coin_a)                           // profit gate
///
/// Atomicity comes for free: a PTB is one transaction, so if `settle` aborts,
/// every swap in the block is reverted. The receipt has **no abilities** (it is a
/// "hot potato"), so the type system forces the caller to consume it via `settle`
/// in the same PTB — there is no way to walk away with the coins and skip the
/// profit check.
///
/// Profitability protection
/// ------------------------
/// `begin` records the *real* value of the supplied input coin. `settle` aborts
/// unless `final_amount >= initial_amount + min_profit`. Gas is intentionally not
/// modeled on-chain (it cannot be known precisely before execution); the
/// off-chain engine bakes the expected gas + safety margin into `min_profit`.
///
/// Flash-loan extension
/// --------------------
/// This same hot-potato shape composes with a flash loan: a lender returns
/// `(Coin<A>, FlashReceipt)`; you run `begin/.../settle_and_return` on the
/// borrowed coin, repay `lender::repay(FlashReceipt, coin)`, then keep the rest.
/// Two hot potatoes in flight, both must be discharged in the PTB — no new
/// executor logic required.
module arbitrage_system::executor;

use sui::coin::{Self, Coin};
use sui::event;

/// `final_amount` did not clear `initial_amount + min_profit`.
const E_INSUFFICIENT_PROFIT: u64 = 1;
/// `initial_amount + min_profit` would overflow u64 (caller passed absurd input).
const E_PROFIT_TARGET_OVERFLOW: u64 = 2;

const U64_MAX: u64 = 18446744073709551615;

/// Hot potato: no `key`/`store`/`copy`/`drop`. Must be consumed by a settle
/// function within the same transaction, which is what enforces the profit gate.
public struct ArbReceipt {
    initiator: address,
    initial_amount: u64,
    min_profit: u64,
}

/// Emitted on a successful settle. Indexers/dashboards key off this.
public struct ArbExecuted has copy, drop {
    initiator: address,
    initial_amount: u64,
    final_amount: u64,
    profit: u64,
}

/// Open an arbitrage session. Returns the input coin untouched (to feed into the
/// first swap) plus the receipt that locks in the profit target.
public fun begin<A>(
    input: Coin<A>,
    min_profit: u64,
    ctx: &TxContext,
): (Coin<A>, ArbReceipt) {
    let initial_amount = coin::value(&input);
    let receipt = ArbReceipt {
        initiator: ctx.sender(),
        initial_amount,
        min_profit,
    };
    (input, receipt)
}

/// Close the session: enforce the profit gate, emit the event, and return the
/// proceeds to the initiator. Reverts the whole PTB if the route was unprofitable.
public fun settle<A>(receipt: ArbReceipt, output: Coin<A>) {
    let ArbReceipt { initiator, initial_amount, min_profit } = receipt;
    let final_amount = coin::value(&output);
    let profit = assert_profit(initial_amount, min_profit, final_amount);
    event::emit(ArbExecuted { initiator, initial_amount, final_amount, profit });
    transfer::public_transfer(output, initiator);
}

/// Same gate as `settle` but returns the coin instead of transferring it, for
/// PTBs that chain the proceeds into a follow-up call (e.g. repay a flash loan)
/// before delivering the remainder to the user.
public fun settle_and_return<A>(receipt: ArbReceipt, output: Coin<A>): Coin<A> {
    let ArbReceipt { initiator, initial_amount, min_profit } = receipt;
    let final_amount = coin::value(&output);
    let profit = assert_profit(initial_amount, min_profit, final_amount);
    event::emit(ArbExecuted { initiator, initial_amount, final_amount, profit });
    output
}

/// Enforce `final >= initial + min_profit` without overflow. Returns the profit.
fun assert_profit(initial_amount: u64, min_profit: u64, final_amount: u64): u64 {
    assert!(min_profit <= U64_MAX - initial_amount, E_PROFIT_TARGET_OVERFLOW);
    assert!(final_amount >= initial_amount + min_profit, E_INSUFFICIENT_PROFIT);
    final_amount - initial_amount
}

// --- read-only accessors (handy for off-chain dev-inspect / tests) ---

public fun initiator(receipt: &ArbReceipt): address { receipt.initiator }
public fun initial_amount(receipt: &ArbReceipt): u64 { receipt.initial_amount }
public fun min_profit(receipt: &ArbReceipt): u64 { receipt.min_profit }
