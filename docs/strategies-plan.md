# Strategies Plan — liquidations (and other non-predatory MEV) as opportunity sources

**Status:** ✅ accepted. Confirmed: (A) funnel health — local prefilter + authoritative
on-chain read; (B) event-driven obligation index + periodic reconcile; (C) Scallop first,
then Suilend, then NAVI behind the same `OpportunitySource`.

The rule: **one pipeline, many sources.** Liquidation is not a second bot — it is a new
`OpportunitySource` that emits the existing `Opportunity` and rides the existing path
(authoritative re-quote → dry-run → `RiskGuard` → submit), through the same profit gate
(`executor::settle_and_return`) and flash/adapter machinery. Phase 0 (done) added the
`OpportunitySource` trait + `ArbSource` + an `OppKind` marker on `Opportunity`; nothing
downstream forked.

---

## Protocol verification (read from source, not memory)

Cloned + inspected current mainnet interfaces:

### Scallop — `protocol` (id `0xefe8b3…`, published-at `0xde5c09…`)
- **Liquidate** (`sources/user/liquidate.move`):
  ```move
  public fun liquidate<DebtType, CollateralType>(
      version: &Version, obligation: &mut Obligation, market: &mut Market,
      available_repay_coin: Coin<DebtType>, coin_decimals_registry: &CoinDecimalsRegistry,
      x_oracle: &XOracle, clock: &Clock, ctx,
  ): (Coin<DebtType> /*remain*/, Coin<CollateralType> /*seized*/)
  ```
  Returns the **collateral coin directly** (no cToken unwrap) + leftover repay. Clean
  fit for our swap adapter.
- **Permissionless?** Gated by `market::assert_whitelist_access`, which calls
  `whitelist::is_address_allowed`. That returns **true for everyone in `allow_all`
  mode** (the mainnet market's normal mode); only `reject_all`/whitelist mode blocks.
  → permissionless in practice; **the bot must check the market's mode at runtime** and
  treat reject/whitelist mode as a non-opportunity.
- **Sizing/close factor:** soft liquidation toward risk=1 via
  `liquidation_evaluator::calculate_liquidation_amounts<Debt,Collateral>(obligation,
  market, registry, x_oracle, clock, available_repay) -> (actual_repay, liq_amount,
  protocol_amount)` — **exposed as an on-chain read** (this is our parity oracle, below).
- **Oracle:** `x_oracle::XOracle` (Scallop's aggregator over Pyth/Supra); price read via
  `price::get_price(x_oracle, type, clock)` with `clock`-based staleness. Whether a fresh
  x_oracle update must be pushed in-PTB before liquidate is a **Phase-2 verification item**.
- **Obligation:** **shared object** (`open_obligation` → `public_share_object`; owner
  holds an `ObligationKey`). Referenceable by id, enumerable via events.

### Suilend — `suilend` (Pyth-based)
- **Liquidate** (`sources/lending_market.move`):
  ```move
  public fun liquidate<P, Repay, Withdraw>(
      lending_market: &mut LendingMarket<P>, obligation_id: ID,
      repay_reserve_array_index: u64, withdraw_reserve_array_index: u64,
      clock, repay_coins: &mut Coin<Repay>, ctx,
  ): (Coin<CToken<P, Withdraw>>, RateLimiterExemption<P, Withdraw>)
  ```
  Returns a **CToken** (needs a redeem step to underlying before swap) + a
  `RateLimiterExemption` to discharge. **Requires in-PTB Pyth `refresh_reserve_price(…,
  price_info: &PriceInfoObject)`** per reserve before liquidate (the classic gotcha).
  Obligations live inside `LendingMarket<P>` (shared), keyed by `ID`.

### NAVI — `lending_core`
- Heavier object set (`Storage`/`Pool`/incentive). **Defer to last.**

**Recommendation: do Scallop end-to-end first.** It returns the collateral coin
directly, has shared obligations, exposes its own sizing math as a read (de-risks
health parity), and we already verified + integrated its flash loan (`ScallopProvider`).
Suilend second (cToken + Pyth-update complexity), NAVI third — both behind the same
trait, no pipeline changes.

---

## Decision A — health/detection approach (the important one)

The task's literal Phase 3 says "replicate each protocol's exact health math in Rust,
parity-tested via devInspect." Scallop (and Suilend) **expose their sizing/health as
on-chain read functions**, which lets us do better and safer:

- **Recommended — funnel, mirroring the arb design:**
  1. *Stage 1 (cheap, local, over-includes):* an approximate health factor from the
     indexed obligation state + oracle prices, to shortlist underwater candidates.
     Wrong-but-conservative is fine here.
  2. *Stage 2 (authoritative):* `devInspect` the protocol's **own**
     `calculate_liquidation_amounts` / obligation-health view for each shortlisted
     obligation → exact `(actual_repay, liq_amount, …)` using the protocol's own oracle
     + math. Emit an `Opportunity` only from this.

  This makes constraint #3 (same oracle, same math) **true by construction** — we call
  their function — and minimizes misfire/gas burn. It's the exact stage-1/stage-2 split
  the arb path already uses (`scanner` → `quoter`).
- *Alternative:* fully reimplement each protocol's borrow-weight / collateral-factor /
  close-factor math in Rust and parity-test it. More code, more drift risk, same result.
  Only worth it if the on-chain read is too slow to call per candidate (it isn't — it's
  one devInspect on the shortlist).

`health.rs` (Phase 3) still exists for the **stage-1 local approximation** and is
parity-checked against the on-chain view; the *authoritative* verdict is always the
on-chain read.

## Decision B — indexing model

- **Recommended — event-driven index + periodic reconcile.** Subscribe to Scallop's
  obligation lifecycle events (open / deposit_collateral / borrow / repay /
  withdraw / liquidate) and maintain a local obligation index in the cache. **Bootstrap**
  by querying historical open-obligation events (`queryEvents`) or a public indexer, then
  reconcile periodically (re-read a sample) to repair missed/gapped events. Document the
  completeness limit (event retention depth; reconcile cadence).
- *Alternatives:* poll an on-chain registry/table (if exposed) — simpler but heavier;
  or rely solely on a third-party indexer — a dependency + trust issue. Event-driven +
  reconcile is the robust middle, consistent with the existing `ws.rs` ingestion.

## Decision C — protocol scope for v1

- **Recommended:** Scallop only for the first end-to-end proof; add Suilend, then NAVI,
  behind the same `OpportunitySource` once the pipeline is proven. Confirm.

---

## How a liquidation maps onto the existing PTB builder

One atomic PTB, every existing gate enforced (collateral seized → swapped back to the
debt/base asset → must clear `initial + min_profit` or revert):

```text
[x_oracle / pyth price update]                                  # if protocol requires fresh price
(repay, frcpt) = flash::borrow_flash_loan(version, market, actual_repay)   # ScallopProvider (optional)
(repay, arcpt) = executor::begin(repay, min_profit)
(remain, seized) = scallop_liquidate_adapter::liquidate<Debt,Coll>(version, obligation, market, repay, registry, x_oracle, clock)
debt_swapped   = <dex>_adapter::swap_exact_in_*(pool, seized, min_out)     # collateral → debt/base (Cetus/Turbos)
proceeds       = join(remain, debt_swapped)
proceeds       = executor::settle_and_return(arcpt, proceeds)              # PROFIT GATE (unchanged)
change         = flash::repay_flash_loan(version, market, proceeds, frcpt) # REPAYMENT GATE (unchanged)
transfer(change, sender)                                                   # keep the bonus
```

- **Sizing:** `actual_repay` comes from the authoritative `calculate_liquidation_amounts`
  read, so `remain ≈ 0` (the liquidate adapter `destroy_zero`s it, or returns it to the
  sender if non-zero).
- **`min_out` on the swap-back** comes from the dry-run (never `0`), via the existing
  per-hop floor logic in `executor.rs`.
- **`Opportunity` generalization (Phase 1):** extend `OppKind::Liquidation` to carry a
  `LiquidationLeg { protocol, obligation_id, debt_type, collateral_type, repay_amount,
  extra_object_ids }`; `route` keeps the swap-back hop(s). The PTB builder prepends the
  liquidate leg before the swap hops — additive, the arb path is untouched.
- **Adapter:** `sources/adapters/scallop_liquidate_adapter.move` normalizes the
  two-coin Scallop return to the adapter convention (exact repay in → seized
  `Coin<Collateral>` out; minimal shared objects; knows nothing about profit).

## Frictions extension (Phase 6)

The liquidation race is **harsher than arb** and modeled separately:
- **Winner-take-all:** only the first liquidator lands; model capture probability much
  lower than arb's `1/(1+competitors)` — closer to a latency-percentile win against
  established, well-optimized bots.
- **Oracle-gated:** the opportunity only opens when a fresh price crosses the threshold;
  capture is tied to our oracle-update + landing latency, not just edge size.
- **Fat tails:** bursty, volatility-driven — a single large liquidation can dwarf a month
  of arb. The model must keep the tail, not just the mean.

`docs/liquidation-pnl.md` (Phase 6) reports measured opportunity flow (real index +
authoritative sizing, **paper mode**) and the frictions-adjusted net, with an honest
go/no-go. Submit stays `submit_enabled=false`.

## Phase 7 — other sources

- **Backrun-arb (in scope, high reuse):** after a large swap event moves a pool, trigger
  the existing arb scan on the freshly-updated graph. It's another `OpportunitySource`
  feeding the same path — minimal new code.
- **JIT liquidity (research-only):** on Sui there is **no public mempool**, so you cannot
  see a pending swap to wrap with just-in-time liquidity in the same checkpoint reliably.
  Feasibility is poor; **do not build** — noted here for the record.
- **Out of scope — predatory sandwiching of user swaps:** excluded on principle
  (non-predatory MEV only) and because, with no public mempool, it is unreliable and a
  countermeasure magnet. Recorded so the boundary is explicit.

---

## Decisions I need from you (constraint #7)

1. **Health/detection (A):** confirm the funnel approach — cheap local pre-filter +
   **authoritative `devInspect` of the protocol's own sizing/health read** as the
   verdict (recommended), vs a full Rust reimplementation of each protocol's math.
2. **Indexing (B):** confirm event-driven obligation index + periodic reconcile.
3. **Scope (C):** confirm Scallop first (then Suilend, then NAVI behind the same trait).

I'll start Phase 1 once you confirm these (or amend them).

---

## Progress log

- **Phase 0 ✅:** `OpportunitySource` trait + `ArbSource` (`strategy.rs`); `OppKind` +
  `LiquidationLeg` on `Opportunity`. Pipeline unchanged.
- **Phase 1 ✅ (code; live):** `liquidation/index.rs` — event-driven Scallop obligation
  index (bootstrap via `query_events`, maintain via the event stream, re-read on change).
- **Phase 2 ✅ (code; live):** `liquidation/oracle.rs` — reads Scallop's own
  `price::get_price` via `devInspect` (same oracle/value the protocol uses) + staleness +
  Hermes VAA fetch seam for Pyth-gated protocols.
- **Phase 3 ✅ (offline, tested):** `liquidation/health.rs` — local HF pre-filter
  (over-includes); authoritative verdict is the protocol's on-chain read.
- **Phase 4 ✅ (offline, tested):** `liquidation/detect.rs` — sizes repay vs swap-back
  slippage (priced by `clmm.rs`); rejects when slippage/gas eats the bonus; emits a
  `Liquidation` `Opportunity`.
- **Phase 5 ✅:** `ptb::liquidation_plan` (offline, tested) + `ptb::build_liquidation`
  (live). **No Move adapter needed for Scallop** — its `liquidate` returns the collateral
  coin directly, so the live assembler composes the move-call directly against the
  published package (same pattern as the flash provider), avoiding a heavy Move dep.
- **Phase 6 ✅:** liquidation-race model in `frictions.rs` (offline, tested); executor
  doc records that liquidation rides the shared dry-run → `RiskGuard` → submit gate;
  `docs/liquidation-pnl.md` (methodology + model + conditional NO-GO/measure-first).
- **Phase 7 ✅:** `BackrunSource` (offline, tested). JIT liquidity = research-only (no
  mempool on Sui). Predatory sandwiching = out of scope (recorded above).
- **Verification:** `cargo test` 51/51 + clippy + fmt clean; `sui move test` 12/12.
  Live modules (`index`/`oracle`/`build_liquidation` + the rest) compile under
  `--features live`; not built/run offline here. A real liquidation leg needs a mainnet
  fork + funded key — documented, not run. No live numbers claimed.
