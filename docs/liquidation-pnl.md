# Liquidation P&L — methodology, model, and go/no-go

**Honesty up front:** this is **not yet a live measurement.** The obligation index +
oracle + authoritative health read are live/SDK-gated and require a mainnet run to
produce real opportunity flow; that run has **not** been executed in this environment.
What follows is (1) the measurement methodology now wired, (2) a frictions model
(code, unit-tested) applied to representative liquidation sizes, and (3) a go/no-go that
is explicitly conditional on the live paper run. No numbers here are claimed as measured.
Submit stays `submit_enabled = false`.

## What is built (and tested offline)

- **Index** (`liquidation/index.rs`, live): event-driven Scallop obligation index +
  reconcile.
- **Oracle** (`liquidation/oracle.rs`, live): reads Scallop's own `price::get_price`
  via `devInspect` — the SAME value `liquidate` uses (constraint #3 by construction).
- **Health** (`liquidation/health.rs`, offline, tested): stage-1 local HF pre-filter;
  authoritative verdict is the protocol's on-chain sizing read.
- **Detect/sizing** (`liquidation/detect.rs`, offline, tested): sizes the repay so the
  seized collateral, **after swap-back slippage priced by `clmm.rs`**, still clears
  flash fee + gas + `min_profit`; rejects when slippage eats the bonus.
- **PTB** (`ptb.rs`): `liquidation_plan` (tested) + `build_liquidation` (live) —
  flash → begin → liquidate → swap-back → settle_and_return → repay → transfer.
- **Frictions** (`frictions.rs`, offline, tested): a liquidation-race model
  (`p_capture_liquidation`, `adjusted_liquidation`).

## How the live paper measurement will work

1. Bootstrap + maintain the Scallop obligation index (mainnet).
2. Each tick: local HF pre-filter → shortlist underwater obligations.
3. For each shortlisted obligation: `devInspect` Scallop's `calculate_liquidation_amounts`
   → authoritative `(actual_repay, seized, …)`; price the swap-back with the live CLMM
   quoter; emit a paper `Opportunity` with net = bonus − swap slippage − flash fee − gas.
4. Log every paper opportunity (size, net, obligation) — **no submit**. Accumulate over
   a volatile window to capture the fat tail.

## Frictions model (the race is harsher than arb)

`p_capture_liquidation = exp(−latency/window) / (1 + competitors)` — winner-take-all,
oracle-gated, heavily contested. With the model defaults (latency 700 ms, window 1500 ms,
5 competitors) capture ≈ **0.106** (vs arb's 1/(1+5)=0.167 for the same competitor count;
liquidation is strictly lower because it must *also* land inside the oracle window).

Illustrative `E[net]` per episode (model, **not measured**), gas $0.05 on a miss:

| Liquidation bonus (paper) | capture | E[net] |
|---|---:|---:|
| $5 (small) | 0.106 | +$0.49 |
| $50 (medium) | 0.106 | +$5.26 |
| $1,000 (large, tail) | 0.106 | +$106 |
| $0.20 (dust) | 0.106 | −$0.02 (gas) |

The shape is the whole point: small liquidations are ~break-even after the race + gas
(and must be filtered out), while a **single large liquidation in a volatile hour can
clear ~$100+ at only ~10% capture** — the fat tail arb does not have.

## Go/no-go (conditional)

- **vs arb:** arb netted ~$1–3/day after frictions ([frictions-adjusted-pnl.md](frictions-adjusted-pnl.md))
  and was a NO-GO. Liquidations have a *structurally better* payoff shape (fat tail), so
  they are the more promising execution candidate — **if** large underwater events occur
  often enough and we can land inside the oracle window against entrenched bots.
- **The open question is frequency × capture, which only the live paper run answers.**
  The model says: profitable iff the large-episode rate × our realistic capture exceeds
  the gas bleed from the many small/dust episodes (which the scanner's `min_profit` +
  `RiskGuard` already filter).
- **Recommendation:** run the live index in **paper mode** over a volatile multi-day
  window; tabulate the realized opportunity distribution and apply this model. **Wire
  real submit only if** the paper tail is both large and frequent enough to beat the
  measured capture rate — and even then, behind `submit_enabled` + the kill switch +
  daily-loss cap. Until that data exists, the verdict is **NO-GO / measure first**, same
  discipline as arb. Capital is never at risk in the interim (paper/dry-run only).

_Model + parameters: `offchain/src/frictions.rs` (liquidation section), unit-tested.
Sizing: `offchain/src/liquidation/detect.rs`. No live numbers claimed._
