/// Generic single-asset flash-loan abstraction + a reference/mock lender.
///
/// PROVIDER CONVENTION (Move has no traits — this is a signature convention, like
/// `adapters/`). A flash-loan provider module MUST expose a borrow/repay pair:
///
///   public fun borrow<T>(lender: &mut Lender<T>, amount: u64, ctx)
///       -> (Coin<T>, FlashReceipt)
///   public fun repay<T>(lender: &mut Lender<T>, receipt: FlashReceipt,
///                       payment: Coin<T>, ctx) -> Coin<T>   // returns the change
///
/// `FlashReceipt` is a **hot potato** (no abilities): the type system forces the
/// borrower to call `repay` in the same transaction, and `repay` asserts the
/// payment covers `amount + fee`. Combined with the executor's `ArbReceipt`, a
/// flash arbitrage PTB carries TWO hot potatoes, both of which must be discharged
/// atomically — there is no path that keeps the borrowed funds or skips the profit
/// check.
///
/// This module ships a working reference lender (`FlashLender<T>`, a shared vault)
/// so the whole borrow→arb→repay flow is testable on-chain end-to-end. Real lenders
/// (Scallop / Navi / Suilend) are wrapped by thin adapter modules that present this
/// same borrow/repay shape over the lender's own flash API (see
/// docs/flash-loan-design.md).
module arbitrage_system::flash;

use sui::balance::{Self, Balance};
use sui::coin::{Self, Coin};
use sui::event;
use arbitrage_system::admin::AdminCap;

/// `payment` did not cover `amount + fee`.
const E_REPAY_TOO_LOW: u64 = 1;
/// `repay` was called against a different lender than the one that issued the loan.
const E_WRONG_LENDER: u64 = 2;
const E_BAD_FEE: u64 = 3;

/// Fee basis points denominator (30 = 0.30%).
const FEE_DENOM: u64 = 10_000;

/// A single-asset flash-loan vault. Shared so anyone can borrow; lends from
/// `reserve` and requires atomic repayment of `amount + fee`.
public struct FlashLender<phantom T> has key {
    id: UID,
    reserve: Balance<T>,
    fee_bps: u64,
}

/// Hot potato: no `key`/`store`/`copy`/`drop`. Must be consumed by `repay` in the
/// same transaction. Binds the debt to the exact lender that issued it.
public struct FlashReceipt {
    lender: ID,
    amount: u64,
    fee: u64,
}

public struct LenderCreated has copy, drop { lender: ID, fee_bps: u64, reserve: u64 }
public struct LoanBorrowed has copy, drop { lender: ID, amount: u64, fee: u64 }
public struct LoanRepaid has copy, drop { lender: ID, amount: u64, fee: u64, change: u64 }

/// Create and share a flash-loan vault seeded with `funds`. Admin-gated.
public fun create_lender<T>(_admin: &AdminCap, funds: Coin<T>, fee_bps: u64, ctx: &mut TxContext) {
    assert!(fee_bps < FEE_DENOM, E_BAD_FEE);
    let lender = FlashLender<T> { id: object::new(ctx), reserve: coin::into_balance(funds), fee_bps };
    event::emit(LenderCreated {
        lender: object::id(&lender),
        fee_bps,
        reserve: balance::value(&lender.reserve),
    });
    transfer::share_object(lender);
}

/// Top up a vault. Admin-gated.
public fun fund<T>(_admin: &AdminCap, lender: &mut FlashLender<T>, funds: Coin<T>) {
    balance::join(&mut lender.reserve, coin::into_balance(funds));
}

/// Borrow `amount` of `T`. Returns the loan coin and a hot-potato receipt that
/// MUST be repaid via `repay` in the same transaction. Aborts (reverting the PTB)
/// if the vault lacks the liquidity.
public fun borrow<T>(lender: &mut FlashLender<T>, amount: u64, ctx: &mut TxContext): (Coin<T>, FlashReceipt) {
    let fee = fee_amount(amount, lender.fee_bps);
    let loan = coin::take(&mut lender.reserve, amount, ctx);
    let lender_id = object::id(lender);
    event::emit(LoanBorrowed { lender: lender_id, amount, fee });
    (loan, FlashReceipt { lender: lender_id, amount, fee })
}

/// Repay a loan. Aborts unless `payment` covers `amount + fee` AND `lender` is the
/// vault that issued the loan. Deposits exactly `amount + fee` back into the vault
/// and RETURNS the change (the arbitrage profit) to the caller. Consumes the
/// hot potato, satisfying its no-drop obligation.
public fun repay<T>(
    lender: &mut FlashLender<T>,
    receipt: FlashReceipt,
    mut payment: Coin<T>,
    ctx: &mut TxContext,
): Coin<T> {
    let FlashReceipt { lender: lender_id, amount, fee } = receipt;
    assert!(object::id(lender) == lender_id, E_WRONG_LENDER);
    let due = amount + fee;
    assert!(coin::value(&payment) >= due, E_REPAY_TOO_LOW);
    let owed = coin::split(&mut payment, due, ctx);
    balance::join(&mut lender.reserve, coin::into_balance(owed));
    event::emit(LoanRepaid { lender: lender_id, amount, fee, change: coin::value(&payment) });
    payment
}

/// Fee owed for borrowing `amount`, rounded UP (the lender never under-charges).
/// Identical formula to the off-chain `flashloan::quote_fee`.
public fun fee_amount(amount: u64, fee_bps: u64): u64 {
    let n = (amount as u128) * (fee_bps as u128);
    (((n + (FEE_DENOM as u128) - 1) / (FEE_DENOM as u128)) as u64)
}

public fun fee_denom(): u64 { FEE_DENOM }

// --- accessors (off-chain dev-inspect / tests) ---
public fun receipt_amount(r: &FlashReceipt): u64 { r.amount }
public fun receipt_fee(r: &FlashReceipt): u64 { r.fee }
public fun receipt_total(r: &FlashReceipt): u64 { r.amount + r.fee }
public fun receipt_lender(r: &FlashReceipt): ID { r.lender }
public fun reserve_value<T>(lender: &FlashLender<T>): u64 { balance::value(&lender.reserve) }
public fun lender_fee_bps<T>(lender: &FlashLender<T>): u64 { lender.fee_bps }
