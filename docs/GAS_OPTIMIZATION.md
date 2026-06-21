# Gas Optimization Checklist

Sui gas = computation + storage + (storage rebate). Arbitrage is latency- and
gas-sensitive, so the design minimizes all three. Status reflects the current code.

## On-chain (Move)

- [x] **Executor creates zero persistent objects.** Only a hot-potato value
      (`ArbReceipt`) that lives and dies inside the PTB — no `object::new`, no
      storage cost.
- [x] **No shared mutable state in the executor.** No config/registry object to
      read or lock → no shared-object contention from our own code.
- [x] **One output coin per swap.** Adapters return a single `Coin<Out>`; the
      final coin is transferred once in `settle`.
- [x] **`Balance<T>` for reserves, not `Coin<T>`.** Pools hold `Balance` (no `UID`
      per unit); coins are minted only at the boundary via `coin::take`.
- [x] **Shared math module.** `math.move` is `inline`-friendly pure functions;
      no duplicated logic across modules.
- [x] **Events are minimal & `copy + drop`.** Only what indexers need
      (`ArbExecuted`, `Swapped`), emitted once.
- [ ] **Drop intermediate dust.** If a hop leaves dust, prefer merging into the
      next input over creating a new owned coin (revisit when wiring real venues).
- [ ] **Avoid `Clock`/oracle reads** on the hot path unless a venue requires it
      (Cetus `flash_swap` needs `&Clock` — batch it once per PTB).

## PTB construction (off-chain)

- [x] **Everything in ONE PTB.** `begin → swaps → settle` is a single
      transaction → one gas charge, atomic.
- [x] **Minimal object set.** Touch only each hop's pool + the base coin. No
      registry lookups.
- [ ] **Stable gas coin selection.** Reuse one large gas coin; avoid smashing
      many small coins (extra input objects = more gas).
- [ ] **Order hops to reduce shared-object contention.** Prefer routes through
      less-contended pools; two arbs hitting the same hot pool serialize.
- [ ] **Set `gas_budget` from the dry-run**, not a fixed constant — overpaying
      budget doesn't cost extra, but right-sizing catches regressions.

## Sizing & submission

- [x] **Dry-run before submit** (`executor.rs`): measures real gas + effects;
      skip submission if `net_profit < min_profit` after measured gas.
- [x] **`min_profit` ≥ expected gas + margin.** Losing trades abort in `settle`,
      so worst case is gas only.
- [x] **Trade-size search.** `scanner` probes a grid of input sizes; bigger isn't
      better once price impact eats the spread.
- [ ] **Batch independent opportunities** sparingly — only if they don't share a
      pool (shared object would serialize them anyway).

## Measurement

- [ ] Record `gas_used` from each dry-run; feed the rolling average back into
      `ARB_GAS_COST` so sizing stays calibrated.
- [ ] Track abort rate of `settle` (E_INSUFFICIENT_PROFIT) — a high rate means
      the cache is stale or competitors are faster.
```
