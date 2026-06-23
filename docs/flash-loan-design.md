# Flash-Loan Support — Design, Security Review & Migration

Execute arbitrage with **borrowed** capital. The bot borrows the input, runs the
route, repays the loan + fee, and keeps the change — all in one atomic PTB. No owned
capital is required beyond gas.

---

## 1. Architecture analysis (existing system)

**Execution flow (owned capital).** Routing is off-chain; the on-chain executor is
stateless and creates one hot-potato value, no persistent objects:

```
(coin, receipt) = executor::begin(input_coin, min_profit)   // records initial value
coin            = <adapter>::swap_exact_in_*(pool, coin, min_out)   // per hop
executor::settle(receipt, coin)                              // profit gate + payout
```

**PTB construction.** `offchain/src/ptb.rs` turns a scanner `Opportunity` into one
`ProgrammableTransaction`: split input, `begin`, one `move_call` per hop (adapter
module chosen by `Dex`), `settle`. One transaction ⇒ atomic.

**`begin` / `settle`.** `begin<A>(input, min_profit, ctx)` reads `coin::value(input)`
as `initial_amount` and returns `(input, ArbReceipt{initiator, initial_amount,
min_profit})`. `settle<A>(receipt, output)` destructures the receipt and calls
`assert_profit(initial, min_profit, final)` which aborts unless
`final ≥ initial + min_profit` (overflow-guarded), emits `ArbExecuted`, and transfers
the proceeds to the initiator. `settle_and_return` is identical but **returns** the
coin instead of transferring — the seam for composing a repay afterwards.

**How `ArbReceipt` is enforced.** `ArbReceipt` has **no abilities** (no
`copy`/`drop`/`store`/`key`). The Move type system therefore forbids dropping or
storing it: the only way to consume it is to pass it to `settle`/`settle_and_return`,
both of which run the profit gate. There is no code path that keeps the coins and
skips the check. Atomicity (single PTB) means a failed gate reverts every swap.

**Where flash-loan integration belongs.** Around the existing flow, not inside it.
The executor stays unchanged: borrow *before* `begin`, repay *after*
`settle_and_return`. `settle_and_return` already existed for exactly this — so no
executor edit was needed.

---

## 2. Flash-loan design

**Provider abstraction.** Move has no traits, so a provider is a **signature
convention** (like `adapters/`): a module exposing

```
borrow<T>(lender: &mut L, amount: u64, ctx) -> (Coin<T>, FlashReceipt)
repay<T>(lender: &mut L, receipt: FlashReceipt, payment: Coin<T>, ctx) -> Coin<T>
```

Off-chain, `offchain/src/flashloan.rs` defines the Rust counterpart:

```rust
pub trait FlashLoanProvider {
    fn fee_bps(&self) -> u64;
    fn quote_fee(&self, amount: u64) -> u64;      // ceil(amount*bps/1e4), matches Move
    fn repay_total(&self, amount: u64) -> u64;     // amount + fee
    fn lender_object_id(&self) -> &str;
    fn borrow_call(&self) -> MoveCallSpec;         // package::module::function
    fn repay_call(&self) -> MoveCallSpec;
}
```

The PTB builder asks the provider only for the fee and the borrow/repay call
coordinates — it never hard-codes a lender. New lenders = new `FlashLoanProvider`
impls registered in `provider_from`; **scanner and PTB logic are untouched**.
`MockProvider` (backed by the in-package `flash` vault) is the working reference.

---

## 3. Move contract changes

`executor.move` **already supports** `borrow → begin → swaps → repay → settle`
(via `settle_and_return`), so it is **unchanged**. New file `sources/flash.move`:

- `FlashLender<phantom T>` — a shared single-asset vault (reference/mock lender).
- `FlashReceipt` — **hot potato** (no abilities) carrying `{lender, amount, fee}`.
- `borrow<T>(lender, amount, ctx) -> (Coin<T>, FlashReceipt)` — lends from reserve.
- `repay<T>(lender, receipt, mut payment, ctx) -> Coin<T>` — asserts the payment
  covers `amount + fee` **and** the lender matches the receipt, deposits exactly
  `amount + fee`, and **returns the change** (the profit).
- `fee_amount(amount, bps)` — ceil, identical to off-chain `quote_fee_bps`.

**Guarantees & security assumptions:**
- *Repayment enforced.* `FlashReceipt` has no `drop`/`store` → it must reach `repay`,
  which asserts `payment ≥ amount + fee` (`E_REPAY_TOO_LOW`). Can't be skipped.
- *Profit enforced.* `ArbReceipt` (unchanged) forces `settle_and_return`, which
  asserts `final ≥ initial + min_profit`. The scanner sets
  `min_profit ≥ flash_fee + gas + threshold`, so clearing the profit gate implies
  the loan is repayable **and** leaves a positive remainder.
- *Lender binding.* The receipt stores the issuing lender's `ID`; repaying a
  different vault aborts (`E_WRONG_LENDER`) — you can't repay a cheap loan with a
  different vault's funds.
- *Atomicity.* Both potatoes are consumed in one PTB; any abort reverts the borrow,
  the swaps, and the repay together.
- *No surplus loss.* `repay` takes exactly `amount + fee` via `coin::split` and
  returns the rest, so the profit is never accidentally donated to the vault.

---

## 4. PTB builder changes

`offchain/src/ptb.rs` now emits, in one PTB:

```
flash::borrow  →  executor::begin  →  swap × n  →  executor::settle_and_return
               →  flash::repay     →  TransferObjects(change, sender)
```

The **plan** (`flash_arb_plan`) is pure data (`Vec<PtbStep>`), unit-tested in the
default build. The **live** assembler (`build`, feature `live`) maps the plan to
`sui_types` commands, threading the loan coin and the two receipts through
`Argument::NestedResult`. Ordering is fixed by construction (borrow first, repay
after the profit gate); everything is one transaction → atomic rollback on any
failure, no intermediate state leakage (the loan coin only exists inside the block).

---

## 5. Profitability logic

`scanner::size_route` now computes, per candidate loan size:

```
flash_fee  = ceil(input * flash_fee_bps / 1e4)      // 0 when flash disabled
net_profit = (output - input) - gas_cost - flash_fee
```

`Opportunity` carries `flash_fee`; routes with `net_profit < min_profit` are
rejected. Owned-capital scans set `flash_fee_bps = 0` and behave exactly as before.

---

## 6. Scanner changes

- **Configurable loan sizes** — the existing `candidate_inputs` ladder *is* the set
  of loan sizes in flash mode; the best size is chosen by net profit.
- **Fee simulation** — `flash_fee_bps` (from `Config::flash_fee_bps`) feeds the fee
  into every candidate's net profit.
- **Reject-after-fees** — a route profitable gross but not after fee+gas is dropped
  (test: a 50% fee kills the micro-spread).
- **Ranking preserved** — still ranked by `net_profit`; only the definition of net
  changed.

---

## 7. Security review

| # | Risk | Finding / mitigation |
|---|------|----------------------|
| 1 | **Repayment bypass** | `FlashReceipt` has no abilities → cannot be dropped/stored; only `repay` consumes it, and `repay` asserts `payment ≥ amount+fee`. No bypass. |
| 2 | **Hot-potato violation** | Two potatoes (`ArbReceipt`, `FlashReceipt`); both must be discharged in the PTB or it won't type-check / will abort. Verified by Move tests. |
| 3 | **PTB ordering bug** | Plan fixes order (borrow→begin→swaps→settle_and_return→repay→transfer). `settle_and_return` (profit gate) runs **before** `repay`, and `min_profit ≥ fee`, so repayment can't starve. Test asserts `borrow < settle < repay`. |
| 4 | **Rounding exploit** | Fee rounded **up** (`div_ceil`) on-chain *and* off-chain (bit-identical) → borrower never under-pays via rounding; off-chain never under-estimates the debt. |
| 5 | **Fee miscalculation** | Single source of truth for the formula (`flash::fee_amount` == `flashloan::quote_fee_bps`), unit-tested for equality at several sizes incl. overflow range (u128 intermediate). |
| 6 | **Reentrancy** | Sui's object model + PTB has no synchronous cross-call reentrancy; the vault's `reserve` is a `Balance` mutated only by `borrow`/`repay`/`fund`. Borrow-then-borrow on the same vault in one PTB is allowed but each loan has its own receipt that must be repaid. |
| 7 | **Surplus donation** | `repay` returns the change; the PTB transfers it to the sender — profit is never left in the vault. |
| 8 | **Wrong-lender repay** | Receipt binds the lender `ID`; mismatched repay aborts (`E_WRONG_LENDER`). |
| 9 | **Provider-specific** | Real lenders (below) return *their own* hot-potato receipt and may require extra version/config objects and have their own fee schedule — see integration notes. The arbitrage logic is unaffected because the PTB just threads the foreign receipt from their `borrow` into their `repay`. |
| 10 | **min_profit too low** | If the off-chain `min_profit` were set below the fee, `settle_and_return` could pass while `repay` still aborts — safe (reverts), but wastes gas. The scanner sets `min_profit ≥ fee + gas + threshold` to avoid it. |

No issue leaves funds at risk: the worst case is an aborted transaction costing gas.

---

## 8. Testing

**Move (`sui move test`) — 12/12 pass**, incl. new `flash_arb_tests`:
- `flash_arb_success_repays_and_profits` — borrow 10 → A→B→C→A → settle_and_return →
  repay 11 → keep ~4 profit; vault reserve grows by the fee.
- `flash_repay_too_low_aborts` — `E_REPAY_TOO_LOW`.
- `flash_unprofitable_aborts_and_rolls_back` — `settle_and_return` aborts
  (`E_INSUFFICIENT_PROFIT`) → whole PTB reverts.
- `flash_repay_wrong_lender_aborts` — `E_WRONG_LENDER` (receipt misuse).

**Rust (`cargo test`) — 21/21 pass**, incl.:
- `flashloan::*` — fee quote is ceil & matches Move; `repay_total`; mock provider
  emits `flash::borrow`/`flash::repay`; registry resolves mock only.
- `scanner::flash_fee_is_subtracted_and_can_reject_routes` — fee accounting + a
  punitive fee rejects the route.
- `ptb::*` — flash plan shape/order (borrow<settle<repay), adapter mapping, owned
  plan has no flash steps.

clippy `-D warnings` clean; `cargo fmt --check` clean.

---

## 9. Migration notes

- **No breaking changes.** Owned-capital execution is unchanged
  (`flash_fee_bps = 0`, `owned_arb_plan`, `executor::settle`). `executor.move` is
  byte-for-byte unchanged.
- **New on-chain module** `arbitrage_system::flash` — included automatically in the
  next `sui client publish` of the package; creates no new always-on state until an
  admin calls `create_lender`.
- **Config** (`.env`): `ARB_FLASH_ENABLED`, `ARB_FLASH_PROVIDER` (default `mock`),
  `ARB_FLASH_FEE_BPS`, `ARB_FLASH_LENDER_ID`.
- **Live builder** compiles under `--features live` with the pinned Sui SDK; it needs
  resolved object refs (lender, pools, venue extras) — same resolution the existing
  live path already performs.
- **Gate unchanged:** do not enable live flash execution for a venue until that
  venue's pricing parity passes (Turbos parity: in-range only).

---

## Connecting real providers (Scallop / Navi / Suilend)

All three expose a flash-loan API shaped like our convention (borrow returns funds +
a hot-potato receipt; repay consumes it). To connect one:

1. **Implement `FlashLoanProvider`** for it (`offchain/src/flashloan.rs`): its
   `package_id`, the borrow/repay `MoveCallSpec`, its `fee_bps`, and the lender/market
   object id. Register it in `provider_from`.
2. **Supply extra objects** the lender's borrow/repay need (e.g. a `Version`/
   `Config`/`Market` shared object). Thread them as additional `ObjectArg`s in the
   live builder's borrow/repay commands (same mechanism `ResolvedHop.extra_objs`
   uses for per-venue swap extras).
3. **Thread the foreign receipt** — the lender returns *its own* receipt type; the
   PTB feeds `borrow`'s receipt output straight into `repay`'s input. The arbitrage
   modules never name that type, so nothing else changes.

What's required per provider (to be filled from each protocol's published package):

| Provider | Need |
|----------|------|
| **Scallop** | flash-loan package id; `flash_loan` / `repay_flash_loan` entry sigs; `Version` + `Market` objects; fee schedule. |
| **Navi** | lending package id; flash-loan entry (borrow + repay) sigs; `Storage`/`Pool`/`incentive` objects; fee. |
| **Suilend** | `suilend` package id; flash borrow/repay sigs; `LendingMarket<P>` object + market index; fee. |

Until those are wired, `MockProvider` (the in-package `flash` vault) is the working,
fully-tested provider for local and testnet end-to-end runs.
