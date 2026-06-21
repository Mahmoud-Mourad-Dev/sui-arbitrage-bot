# Architecture

Atomic arbitrage + liquidity routing on Sui, tuned for micro-spreads and minimal
gas. Two layers, one invariant.

```
                          off-chain (Rust)                         on-chain (Move)
 ┌──────────────────────────────────────────────────┐   ┌──────────────────────────────┐
 │  ws.rs        bootstrap + subscribe pool events   │   │  arbitrage_system             │
 │   │           (Cetus / Turbos / amm_v2)           │   │                               │
 │   ▼                                               │   │  executor   (stateless gate)  │
 │  cache.rs     RwLock<HashMap> reserve cache  ◀────┼── │   begin / settle (hot potato) │
 │   │                                               │   │                               │
 │   ▼                                               │   │  adapters/  (one per venue)   │
 │  scanner.rs   build token graph, enumerate        │   │   amm_v2_adapter              │
 │   │           cycles, simulate x*y=k, size trade  │   │   cetus_adapter   (seam)      │
 │   ▼                                               │   │   turbos_adapter  (seam)      │
 │  ptb.rs       build PTB: begin → swaps → settle   │──▶│  amm_v2     reference CP AMM  │
 │   ▼                                               │   │  math       x*y=k (shared)    │
 │  executor.rs  dry-run, submit iff profitable      │   │  admin      AdminCap          │
 └──────────────────────────────────────────────────┘   └──────────────────────────────┘
```

## The one on-chain invariant

A profitable arbitrage is just: *end with more of the base token than you
started with, by at least `min_profit`.* The executor enforces exactly that and
nothing else:

```move
let (coin, receipt) = executor::begin(input, min_profit, ctx); // records initial value
... swaps via adapters ...
executor::settle(receipt, final_coin);                         // asserts final ≥ initial + min_profit
```

`ArbReceipt` is a **hot potato** (no `key`/`store`/`copy`/`drop`): the type system
forces it to be consumed by `settle` in the same transaction. There is no code
path that takes the coins and skips the profit check.

## Why a PTB, not a monolithic Move entry

Routing logic lives off-chain and is assembled as a **Programmable Transaction
Block** — a sequence of Move calls in one atomic transaction where each call's
output feeds the next:

```
split(input) ─▶ executor::begin ─▶ adapter::swap (A→B) ─▶ adapter::swap (B→C) ─▶ adapter::swap (C→A) ─▶ executor::settle
```

Consequences:
- **Atomicity is free.** A PTB is one transaction; if `settle` aborts every swap
  reverts and only gas is paid.
- **No AMM is hardcoded.** The executor never imports an AMM. Each hop is an
  independent `move_call` chosen off-chain.
- **Minimal footprint.** Only the touched pool objects and the base coin are read
  /written. No registry, no shared config, no intermediate owned objects.

## Adapter architecture (extensibility)

Move has no interfaces, so adapters are a **signature convention** (see
`sources/adapters/amm_v2_adapter.move`). Every venue exposes:

```move
public fun swap_exact_in_a_to_b<A, B>(pool: &mut Pool, coin_in: Coin<A>, min_out: u64, ctx): Coin<B>
public fun swap_exact_in_b_to_a<A, B>(pool: &mut Pool, coin_in: Coin<B>, min_out: u64, ctx): Coin<A>
```

Adding **Cetus, Turbos, or any future UniV2-style pool** = adding a new adapter
module + a `Dex` enum variant off-chain. The executor and scanner are untouched.
`cetus_adapter` / `turbos_adapter` ship as documented seams (they `abort` until
their interface package is pinned in `Move.toml`), so the package always compiles.

## Off-chain engine

- **`ws`** hydrates the cache, then streams `amm_v2::Swapped` / adapter events to
  keep reserves current.
- **`cache`** is a single `RwLock<HashMap>`; the scanner takes a lock-free
  snapshot so cycle search never blocks ingestion.
- **`scanner`** builds a directed token graph (two edges per pool), enumerates
  simple cycles `base → … → base` up to `max_hops`, simulates each over a grid of
  input sizes with the **exact** `amm` math (identical to `math.move`), subtracts
  the gas estimate, and returns the most profitable route clearing `min_profit`.
- **`ptb` + `executor`** build the PTB, **dry-run** it (the off-chain gas
  estimate the spec requires — gas is never modeled on-chain), and submit only if
  the simulated result still clears the bar. `settle` is the final backstop.

## Flash-loan extensibility

The hot-potato shape composes. A flash loan returns `(Coin<A>, FlashReceipt)`;
both `FlashReceipt` and `ArbReceipt` are hot potatoes that must be discharged in
the same PTB:

```
(loan_coin, loan)  = lender::flash_loan<A>(pool, amount)
(coin, arb)        = executor::begin(loan_coin, min_profit, ctx)
... swaps ...
proceeds           = executor::settle_and_return(arb, coin)   // gate, keep coin
repay              = coin::split(&mut proceeds, amount + fee)
lender::repay(loan, repay)                                    // discharge loan
transfer(proceeds, sender)                                    // keep the rest
```

No change to the executor — `settle_and_return` already exists for exactly this.
With ~$150 of working capital, flash loans are the path to size: borrow, capture
the spread, repay, keep the delta, all atomic.

## Budget framing ($150)

- One-time: testnet is free; mainnet publish is a few cents of gas.
- Per attempt: a failed (unprofitable) PTB costs only gas (~0.002–0.01 SUI).
  `min_profit` is set above expected gas so losing trades abort in `settle`.
- The capital cap mostly limits trade *size* (price impact), not the number of
  attempts — hence the focus on micro-spreads and flash-loan sizing later.
```
