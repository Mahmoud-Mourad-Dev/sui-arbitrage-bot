# Consolidation Plan — one canonical architecture

**Status:** ✅ accepted (2026-06-23). Confirmed: all-Rust funnel, event-triggered re-read
ingestion, v1 = Cetus + Turbos (defer Bluefin, exclude Momentum).

## The problem

There are two parallel, unconnected implementations:

| | Rust crate (`offchain/`) | Python suite (`validation/cetus/`) |
|---|---|---|
| Pricing | V2 `x*y=k` only (`amm.rs`); CLMM engine (`clmm.rs`) **exists, proven, but unwired** | Authoritative on-chain quoters via `devInspect` for Cetus/Turbos/Bluefin/Momentum |
| Data model | `PoolState{reserve_a,reserve_b,fee_bps}` — no CLMM state | per-venue dicts (`sqrt_price/liquidity/ticks/...`) |
| Ingestion | `ws.rs` stubbed (`decode_reserves → None`) | RPC reads + `devInspect` snapshots, working |
| Execution | `executor.rs::try_execute` builds PTB, dry-run/submit commented out | none — read-only research only |
| Venues known | `Dex{AmmV2,Cetus,Turbos}` | Cetus, Turbos, Bluefin, Momentum, DeepBook, Aftermath, Kriya |
| What it produced | unit tests | **the only real measured result** (`authoritative-pnl-report.md`: ~$17/day active, ~$0 overnight, −$22.8k if you execute everything) |

The honest research lives in Python; the execution skeleton lives in Rust. Neither is
end-to-end. This plan picks **one** architecture and makes Python the *offline oracle*,
not a second runtime.

---

## Decision 1 — where authoritative CLMM quoting lives

**Recommendation: a three-stage funnel, entirely in Rust.** Port the Python
`devInspect` quoter pattern into the Rust SDK (`dev_inspect_transaction_block` — the
SDK builds the PTB, so no hand-rolled BCS like the Python `cetus_rpc.py`). Keep the
Python suite as the **parity reference + research oracle only** (it stays the thing we
diff against; it never runs in the hot path).

```
                       cost / call     authority      runs on
1. clmm.rs engine      ~µs, in-proc    proven vs V2   every (route × size) candidate   → coarse rank
2. devInspect quoter   1 RPC (batched) venue truth    survivors of stage 1             → honest rank + sizing
3. full-PTB dry_run    1 RPC           ground truth   the single best candidate        → submit gate
   on-chain settle/repay              capital backstop the landed tx                   → final
```

Why this split:
- **Stage 1** is the only thing fast enough to scan thousands of detections/hour (the
  report saw 8,751 in 11.4h). The engine is already proven to generalize V2 (`clmm.rs`
  `single_range_reduces_to_v2`) and to match Cetus on-chain exactly (P4, 1034/1034).
- **Stage 2** is the report's core lesson: *engine estimates over-detect; authoritative
  pricing makes net P&L honest.* `snapshot_and_quotes` in `cetus_rpc.py` already does
  the whole route's quotes in **one** atomic `devInspect` from a single state snapshot —
  we port exactly that, so a 3-hop route = 1 RPC, not 3.
- **Stage 3** prices the actual PTB (fees, gas, ordering) and is the existing
  `executor.rs` design; `settle`/`repay` remain the capital backstop.

**Tradeoff:** porting the quoters to Rust duplicates logic that already works in Python.
Alternative was "shell out to Python / keep Python as a sidecar service" — rejected:
two languages in the hot path, IPC latency on the critical race (Phase 4), and a second
deploy artifact. One Rust binary is simpler and faster. The Python code isn't wasted —
it becomes the cross-checking oracle the Rust quoter must match (≤1 unit, the existing
P4 bar).

## Decision 2 — single data model

Generalize `PoolState` with a `PoolKind`, one entry per **(pair, venue, fee tier)** edge
(CLMMs have multiple pools per pair — audit assumption E):

```rust
enum PoolKind {
    V2 { reserve_a: u64, reserve_b: u64 },           // amm.rs path, unchanged
    Clmm(clmm::ClmmState),                            // sqrt_price, liquidity, fee_pips, ticks[]
}
struct PoolState { id, dex, token_a, token_b, fee_bps, kind: PoolKind, last_seq: u64 }
```

- `scanner::simulate` dispatches on `kind`: V2 → `amm::get_amount_out` (bit-identical,
  constraint #2 preserved), CLMM → `clmm::quote_exact_in`.
- `reserves_from`/`other` stay for V2; CLMM adds a `clmm_state()` accessor.
- `last_seq` (object version / checkpoint) drives staleness + gap detection (audit §7).

This is the audit's `PoolCore` collapsed into one Rust type, so there is exactly one
model from ingest → cache → scan → PTB.

## Decision 3 — venues in scope for v1

**Recommendation: Cetus + Turbos only** (plus the in-package `amm_v2` for tests).

- The PnL report shows **Cetus↔Turbos** is the top DEX combination ($1.09 mean, the
  richest), and SUI/USDC Cetus↔Turbos is the single best route.
- **Cetus P4 parity already passed exactly**; Turbos parity passed in-range (multi-tick
  still open — we gate Turbos to in-range quotes until its P4 closes, per the audit rule
  "no adapter ships before P4 passes").
- **Bluefin**: appears in many cycles but as a *third* venue; defer to v1.1 (it's not in
  the Rust `Dex` enum yet — adding it is mechanical once the pattern exists).
- **Momentum**: **exclude** — the report flags a broken USDB/USDC pool quoting ~0 that
  would total-loss the input. Goes straight onto the Phase-6 blacklist.

---

## How the phases land on this architecture

- **Phase 1** — Decision 2 model + Stage-1 engine dispatch in `scanner`; Stage-2
  authoritative re-price of the best candidate. Acceptance test diffs engine vs the
  ported quoter within the P4 tolerance.
- **Phase 2** — real `cetus_adapter` then `turbos_adapter` (flash_swap/repay), pinned in
  `Move.toml`; `ptb.rs::ResolvedHop.extra_objs/type_args` carry `GlobalConfig`/`Clock`/
  `Versioned` / fee-tier type param.
- **Phase 3** — `ws.rs` `bootstrap_pools` + decode into the Decision-2 model; CLMM
  ingestion = **re-read changed pool objects on swap/liquidity events** + periodic
  reconcile (see open question below).
- **Phase 4** — `executor.rs` real dry_run + submit; per-hop `min_out` floors from the
  dry-run to defuse the dry-run→land race; fresh re-quote inside a freshness budget.
- **Phase 5** — one real flash provider (Scallop/Navi/Suilend) behind the existing trait.
- **Phase 6** — frictions model on the paper baseline → `frictions-adjusted-pnl.md` with
  an honest go/no-go; pool blacklist; kill switch + structured decision logging.

---

## Open decisions I need from you (constraint #6)

1. **Quoting model** — confirm the Rust three-stage funnel (engine coarse → ported
   `devInspect` quoter → full-PTB dry-run), with Python demoted to offline parity oracle.
   *Alternative:* keep Python as a live sidecar service. I recommend the all-Rust funnel.
2. **CLMM ingestion model** — CLMMs don't emit reserve deltas. Pick one:
   - **(a) Event-triggered object re-read** *(recommended)* — subscribe to swap/liquidity
     events, then re-read the changed pool object + tick window via `multiGetObjects`.
     Simple, robust, slightly more RPC.
   - **(b) Per-checkpoint diff** — lower latency, much more plumbing and RPC volume.
   - **(c) Decode swap-event payloads directly** — least RPC, but venue-specific and
     fragile (must reconstruct `sqrt_price/liquidity/tick` from each venue's event schema).
3. **v1 venue scope** — confirm Cetus + Turbos, defer Bluefin, exclude Momentum.

I'll start Phase 1 once you confirm these three (or amend them).

---

## Progress log

- **Phase 1 ✅ (verified):** `PoolKind{V2,Clmm}` model; scanner dispatches per kind
  (V2→`amm`, CLMM→`clmm`); `Quoter`/`EngineQuoter`/`reprice_route` stage-2 seam.
  Rust 24/24 tests (CLMM triangle matches V2 within tolerance; balanced CLMM →
  none), clippy + fmt clean.
- **Phase 2 ✅ (on-chain verified):** real `cetus_adapter` (flash_swap/repay_flash_swap)
  and `turbos_adapter` (swap_router, `<A,B,FeeType>`), built against pinned **mainnet**
  Cetus (`mainnet-v1.52.3`) + Turbos (commit `cff6932`) interfaces. `Move.toml` switched
  to `framework/mainnet` with Sui/MoveStdlib overrides. `sui move test` 12/12.
  PTB builder: `ptb::live::ResolvedHop` carries per-venue extras (GlobalConfig/Clock/
  Versioned) and `build` assembles each hop's args in its adapter's exact order.
- **Phase 3 ✅ (code; SDK-gated):** `quoter.rs` (authoritative `devInspect` re-quote —
  Cetus `calculate_swap_result`, Turbos `pool_fetcher::compute_swap_result` — funnel
  stage 2) and a rewritten `ws.rs` (real `multi_get_object` bootstrap → decode CLMM
  pool → `PoolState` + `LivePoolRef`; event-triggered object re-read).
- **Phase 4 ✅ (code; SDK-gated):** `executor.rs` — authoritative re-quote → per-hop
  `min_out` floors (race guard) → dry-run gate → `RiskGuard` → submit **only if
  `submit_enabled`** (off by default). `apply_slippage_floor`/`mist_to_usd` unit-tested.
- **Phase 5 ✅ (verified):** `ScallopProvider` (real `borrow_flash_loan`/
  `repay_flash_loan`, package `0xde5c09…`) behind the `FlashLoanProvider` trait +
  `FlashStyle`; registered in `provider_from`. Unit-tested in the default build.
- **Phase 6 ✅ (verified + doc):** `frictions.rs` (latency/competition model) and
  `risk.rs` (kill switch, daily-loss cap, pool blacklist, realized-vs-predicted) —
  both unit-tested; `docs/frictions-adjusted-pnl.md` with an explicit **NO-GO**.
- **Verification boundary:** `quoter`/`ws`/`executor`/`ptb::live` are written to the
  real Sui SDK + venue APIs and compile under `--features live` (which pulls the whole
  Sui SDK from git — heavy, not built in offline CI here); a real submit also needs a
  funded mainnet key. Flagged per constraint #5 rather than claimed working. All
  offline-compilable code (scanner, providers, frictions, risk) is `cargo test` green.
