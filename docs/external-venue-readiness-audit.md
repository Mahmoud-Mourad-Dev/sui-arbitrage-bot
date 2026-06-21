# External Venue Readiness Audit

**Scope:** can the current scanner (a single `x*y=k` simulator over total reserves)
price swaps on Cetus, Turbos, and Kriya correctly? **Verdict: no** for any
concentrated-liquidity (CLMM) pool. This document proves why, designs the CLMM
pricing engine, quantifies the error, and lays out the migration. No swap adapters
are written; the deliverable is a **mathematically proven pricing engine**
(`offchain/src/clmm.rs`, 6 passing proofs) plus the plan to close empirical parity.

---

## 0. TL;DR

| Venue | Pool model | Current `x*y=k` sim valid? |
|-------|------------|----------------------------|
| **Cetus** | CLMM (concentrated, sqrtPriceX64 + ticks) | ❌ No |
| **Turbos** | CLMM (concentrated, sqrtPriceX64 + ticks) | ❌ No |
| **Kriya** | classic AMM **and** CLMM | ✅ for the classic AMM pools · ❌ for CLMM pools |

The killer fact (Section 5): arbitrage profit is a *small residual*
`profit = out − in`, so an output-estimate error `δ` becomes a profit error of
`δ / margin`. For the micro-spreads this bot targets (margin ≈ 0.05–0.5%), even a
sub-1% output error flips the sign of the decision. On CLMM pools `δ` is not
sub-1% — it is unbounded. **The V2 simulator is decision-inverting on CLMMs.**

---

## 1. Current scanner assumptions vs reality

`scanner.rs` + `amm.rs` encode these assumptions:

| # | Assumption | Cetus | Turbos | Kriya-AMM | Kriya-CLMM |
|---|------------|:-----:|:------:|:---------:|:----------:|
| A | A pool = `(reserve_a, reserve_b, fee_bps)` | ❌ | ❌ | ✅ | ❌ |
| B | Price impact follows `x*y=k` on those reserves | ❌ | ❌ | ✅ | ❌ |
| C | Output is a closed-form, single-shot `get_amount_out` | ❌ | ❌ | ✅ | ❌ |
| D | Liquidity is uniform across all prices | ❌ | ❌ | ✅ | ❌ |
| E | One pool per token pair | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| F | The pool's token *balances* are the swappable depth | ❌ | ❌ | ✅ | ❌ |
| G | A flat `(a,b)` cache, updated atomically, is sufficient | ❌ | ❌ | ✅ | ❌ |

- **A–D, F** fail for every CLMM: depth lives in tick ranges, not in total
  balances; the curve is piecewise; output requires iterating across ticks.
- **E** (⚠️ everywhere): all three venues allow **multiple pools per pair**
  (different fee tiers / tick spacings). The scanner must model each
  `(pair, venue, feeTier)` as a **distinct edge**, not one edge per pair.
- **F** is the most dangerous: a concentrated pool can hold small balances yet
  offer huge depth at spot (or large balances with thin depth at spot). Using
  balances as `x*y=k` reserves is uncorrelated with reality.

> Kriya nuance: KriyaDEX ships a constant-product **spot AMM** (where our V2 sim is
> valid — verify only the fee model/protocol-fee split) **and** a separate
> **CLMM**. Treat them as two different venues in the model.

## 2. Every place `x*y=k` mis-estimates on a CLMM

1. **Reserve source.** `get_amount_out(amount, reserve_in, reserve_out, fee)` —
   there is no correct value to pass for `reserve_in/out` on a CLMM. Balances ≠
   virtual reserves; virtual reserves are only valid for the *current* tick range.
2. **Slippage curve.** `x*y=k` assumes liquidity from price 0→∞. CLMM concentrates
   it; near concentration, real slippage ≪ V2 (output **under**estimated); through
   thin/gappy liquidity, real slippage ≫ V2 (output **over**estimated).
3. **No tick crossing.** Large trades cross ticks where `L` jumps. A single
   hyperbola cannot represent liquidity cliffs or gaps → wrong for any size that
   leaves the active range.
4. **Fee tiers / multiple pools.** Routing must pick the *best* `(pair,feeTier)`
   pool; collapsing to one edge per pair mis-prices and mis-routes.
5. **Directional asymmetry.** Down-swaps and up-swaps traverse different tick
   arrays; the answer is not symmetric the way a single `(a,b)` curve implies.
6. **Trade sizing.** `scanner::find_best` grids input sizes assuming a smooth
   `x*y=k` profit curve; on a CLMM the profit curve is piecewise and can have
   local kinks at tick boundaries — the optimizer must sample the real engine.

## 3. CLMM simulation engine (implemented: `offchain/src/clmm.rs`)

**Fixed point.** `sqrtPriceX64 = √(price)·2^64`, Q64.64, `price = token1/token0`.
(Sui uses **X64**; Ethereum/UniV3 uses X96 — do not mix the scaling.)

**Within a single range** (constant `L`), the pool is `x*y=k` on **virtual
reserves**:

```
x (token0) = L·2^64 / √P        y (token1) = L·√P / 2^64        x · y = L²
```

**Amount ↔ price (UniV3 SqrtPriceMath, ported to X64):**

```
Δx = L·2^64·(√P_hi − √P_lo) / (√P_hi·√P_lo)      (token0)
Δy = L·(√P_hi − √P_lo) / 2^64                     (token1)
√P_next(token0 in) = L·2^64·√P / (L·2^64 + Δx·√P) (price ↓, round up)
√P_next(token1 in) = √P + Δy·2^64 / L             (price ↑)
```

**Swap = tick-crossing loop.** From the current `√P`, step toward the next
initialized tick (or the price limit); `computeSwapStep` fills as much as the
input allows; if it reaches the tick, apply `liquidityNet` (subtract going down,
add going up) and continue:

```
while remaining > 0 and √P ≠ limit:
    target = next initialized tick in direction
    (√P, in, out, fee) = computeSwapStep(√P, target, L, remaining, fee_pips)
    remaining -= in + fee ;  amount_out += out
    if √P == tick:  L ±= liquidityNet(tick)   else break
```

**Inputs the engine needs:** `√P`, active `L`, `fee_pips`, and the **tick table**
(initialized ticks with `liquidityNet` and precomputed `√P` boundary) along the
path. All multiplies use `U256` then narrow — no overflow.

**Fee growth is NOT part of pricing.** `feeGrowthGlobal/Outside` are LP accounting
(who is owed how much). Swap **output** depends only on the static fee rate
(`fee_pips`). Tracking fee growth in a pricing engine is a category error; we
consume only the fee tier. (We *will* read tick `liquidityNet`, which is separate.)

## 4. V2 vs CLMM — and the bridge between them

| | V2 (`amm.rs`) | CLMM (`clmm.rs`) |
|---|---|---|
| Curve | one hyperbola, prices 0→∞ | piecewise `x*y=k`, per tick range |
| State | `reserve_a, reserve_b, fee` | `√P, L, fee, ticks[]` |
| Output | closed form, O(1) | iterate ticks, O(#ticks crossed) |
| Depth source | total balances | active `L` (+ tick map) |
| Cache | `(a,b)` | `√P/L/tick` + tick window |

**Bridge / proof of generalization:** a V2 pool is *exactly* a CLMM with one
infinite range and `L = √(reserve_a·reserve_b)` constant everywhere. Then virtual
reserves equal real reserves and the CLMM math collapses to `x*y=k`. This is
proven in code: `clmm::tests::single_range_reduces_to_v2` shows
`quote_exact_in` reproduces `amm::get_amount_out` to within integer rounding
(≤ a few units on 1e9–1e12 outputs), across 1:1 and 1:4 pools and several
fees/sizes. **So any divergence on a real CLMM is due purely to liquidity
concentration / tick structure — not the formula.**

## 5. Maximum profitability error if V2 is used on Cetus

Let `out_V2`, `out_true` be the estimated/real outputs of one hop, and
`δ = (out_V2 − out_true)/out_true`. Two compounding failure modes:

**(a) Liquidity mapping is unbounded.** V2 slippage ≈ `amountIn / reserve`; CLMM
slippage ≈ `amountIn / (depth at √P)`. There is *no fixed ratio* between a pool's
balances and its depth at spot, so `|δ|` has no upper bound.

**(b) Profit amplifies the error.** Profit margin `m = (out − in)/out`. Then

```
profit_error ≈ δ · out / profit = δ / m
```

For micro-spread arbitrage `m ≈ 0.05–0.5%`. The decision flips sign once
`|δ| ≳ m`.

**Worked Cetus examples:**

| Pool | True slippage | V2 (on balances) predicts | δ (output) | margin m | profit error δ/m | Effect |
|------|---------------|---------------------------|-----------|---------|------------------|--------|
| USDC/USDT, deep concentrated, $50k trade | ~2 bps | ~1.7% (sees small balances) | **−1.6%** | 0.05% | **≈ 3200%** | predicts loss → **skips a real profit** |
| Volatile pair, liquidity offset from spot, $50k | ~8% | ~1.5% | **+6.5%** | 0.3% | **≈ 2000%** | predicts profit → **submits a loser** |

**Conclusion:** the maximum profitability error using V2 on Cetus is effectively
**unbounded and routinely sign-inverting (≫100%)**. The on-chain `settle` gate
still protects *capital* (a misjudged trade aborts, costing only gas), but the
*strategy* is economically non-functional: it skips winners and burns gas on
losers. CLMM pricing is a hard prerequisite, not an optimization.

## 6. Migration plan: `amm_v2`-only → multi-venue production

Each phase has an exit gate; do not start the next until it passes.

- **P0 — done.** amm_v2 testnet-validated (sim == exec, 0.000% error).
- **P1 — pricing engine (this audit).** `clmm.rs` implemented + proven to
  generalize V2 and satisfy invariants. ✅ Gate: 6/6 CLMM proofs green.
- **P2 — venue-agnostic quoting.** Generalize `PoolState` into
  `enum Venue { V2{reserves}, Clmm(ClmmState) }`; replace the hardcoded
  `get_amount_out` in `scanner` with a `quote(pool, amount, dir)` dispatch; make
  trade sizing sample the real engine (no smooth-curve assumption). Gate: scanner
  unit tests on mixed V2+CLMM graphs.
- **P3 — read-only state readers (NOT swap adapters).** Per venue, decode pool
  core (`√P/L/tick/fee`) and the tick table from chain via RPC, hydrating the
  caches in Section 7. Gate: cache reflects on-chain state for N live pools.
- **P4 — empirical parity (the real proof for real DEXes).** For each venue,
  diff `clmm::quote_exact_in` against the venue's **own on-chain quoter**
  (Cetus `pool::calculate_swap_result` / equivalent, via `dev_inspect`) over many
  pools × sizes × directions. **Gate: ≤ 1 unit error** — the same bar amm_v2
  cleared on testnet. Only after this do we trust the engine for a venue.
- **P5 — swap adapters (the currently deferred part).** Implement
  `cetus_adapter` / `turbos_adapter` / `kriya_adapter` swap calls + wire into the
  PTB builder; keep the dry-run + `settle` gates. Gate: testnet sim==exec per venue.
- **P6 — production hardening.** Risk limits, hot-wallet isolation, monitoring,
  multi-venue routing across all edges.

**Rule:** no swap adapter for a venue ships before P4 passes for that venue.

## 7. Cache, tick cache, WebSocket, refresh

### Pool cache (per pool)
```
PoolKey   = (venue, pool_id)
PoolCore  = { token0, token1, fee_pips, venue_kind,
              // V2:
              reserve0?, reserve1?,
              // CLMM:
              sqrt_price?, liquidity?, current_tick?, tick_spacing?,
              last_seq /* object version or checkpoint for ordering */ }
```
Indexed by token for graph building; one entry per `(pair, feeTier)` edge.

### Tick cache (per CLMM pool)
```
TickMap = sorted map  tick_index -> { liquidity_net:i128, liquidity_gross:u128,
                                      sqrt_price:u128 /* precomputed boundary */,
                                      initialized:bool }
+ a tick-bitmap (or sorted index) to find the next initialized tick fast
+ window = [tick_lo, tick_hi] actually loaded around current_tick
```
- Store `sqrt_price` per tick **precomputed** so the engine never needs `1.0001^x`
  on the hot path.
- Keep a **window** around spot (e.g. ±K·tick_spacing sized to the largest trade
  we'd attempt). If a simulated swap would exit the window, mark the quote
  *depth-limited* and trigger a fetch rather than trusting an extrapolation.

### WebSocket subscriptions (`suix_subscribeEvent`)
- Per venue, subscribe by **MoveModule** event filter on the CLMM pool module,
  narrowed to tracked pool ids:
  - **swap** events → update `√P, current_tick, liquidity` (hot, frequent).
  - **add/remove liquidity (modify position)** events → update affected ticks'
    `liquidity_net/gross` in the tick cache.
  - **pool created** → discover new edges.
- Kriya: also subscribe to classic-AMM reserve/swap events for its spot pools.
- WS is best-effort and gap-prone → never the sole source of truth.

### Refresh strategy
- **Event-driven (hot):** apply swap/liquidity deltas incrementally.
- **Reconcile (warm):** periodic full re-read of pool core + the tick window via
  `multiGetObjects` every K seconds or N events, to correct missed/zipped events.
- **Lazy tick extension:** widen the window on demand when a trade nears its edge
  or a liquidity event lands just outside it.
- **Ordering & gap detection:** key every update by Sui **object version /
  checkpoint seq**; if seq jumps, force a reconcile for that pool.
- **Staleness guard:** tag each snapshot with `last_seq` + timestamp; if a chosen
  pool is older than a threshold or the swap exits the tick window, re-fetch
  before building the PTB. The on-chain `settle` gate remains the final backstop.

## 8. Pricing-engine correctness — what is proven now, what closes it

**Proven now (in `offchain/src/clmm.rs`, `cargo test`):**
1. `single_range_reduces_to_v2` — the engine reproduces the **testnet-validated**
   V2 math to within rounding ⇒ the X64 SqrtPriceMath port is correct on the curve
   both simulators share.
2. `more_input_more_output`, `higher_fee_lower_output`,
   `deeper_liquidity_less_slippage` — monotonicity in input, fee, and depth.
3. `tick_crossing_adds_slippage` — liquidity cliffs reduce output (the effect V2
   structurally cannot represent).
4. `v2_on_balances_misprices_concentrated_pool` — quantifies Section 5's claim.

**What closes "correct for real Sui DEXes" (P4, deferred per "no adapters yet"):**
bit-exact diff against each venue's on-chain quoter via `dev_inspect`, ≤ 1 unit
error, across a grid of pools/sizes/directions. This requires read-only venue
state decoders (P3), which are not swap adapters. Until P4 passes for a venue, its
quotes are *internally* proven but not *venue*-certified, and no capital routes
through it.

---

### Reproduce
```bash
cd offchain
cargo test clmm                 # 6 CLMM proofs
cargo test                      # full suite (13)
cargo clippy --all-targets -- -D warnings && cargo fmt --check
```
