# Frictions-Adjusted P&L — go/no-go for live execution

The paper baseline (`authoritative-pnl-report.md`) prices every leg authoritatively but
assumes we **always land, instantly, and alone**. This document applies a frictions
model (latency + competition + gas-on-miss) to that baseline and gives an explicit
recommendation. The model is code, not prose: `offchain/src/frictions.rs` (unit-tested),
so it can be re-run on real per-episode data as it accrues.

## Model (see `frictions.rs` for the exact functions)

For an opportunity of size `edge_bps` worth `paper_profit`:

```
p_alive = exp(-latency_ms / (halflife_ms_per_bp · edge_bps))   # edge still there at submit
p_win   = 1 / (1 + competitors)                                # we land first
E[net]  = p_alive · ( p_win · paper_profit − (1 − p_win) · gas )
```

Rationale: bigger dislocations persist longer (survival ∝ edge); we only submit if the
fresh dry-run still clears `min_profit` (≈ `p_alive`); if we then lose the race, the
on-chain `settle` gate reverts us for **gas only** (never principal). Every parameter is
an explicit assumption — none is tuned to a desired answer.

## Inputs from the paper report (active hours)

- 122 deduped episodes, **$8.14** total paper (~$17/day active), **~$0 overnight**.
- Highly concentrated: top route = 30%, top 3 = 52% of profit.
- Representative points: top opp **29.4 bps → $2.14**; the long tail is **thin**
  (mean ≈ $0.067/episode, i.e. a few bps and a few cents).

## Scenario results (model applied)

Capture = adjusted / paper. "Top opp" = the 29.4 bps / $2.14 episode; "thin tail" = a
5 bps / $0.06 episode (representative of most of the 122).

| Scenario | latency | competitors | gas | Top-opp capture | Thin-tail E[net] |
|----------|--------:|------------:|----:|----------------:|-----------------:|
| Optimistic | 400 ms | 1 | $0.02 | ~48% (+$1.03) | ≈ +$0.005 |
| **Base**   | 800 ms | 3 | $0.03 | ~21% (+$0.46) | **−$0.004 (loss)** |
| Pessimistic| 1500 ms| 8 | $0.03 | ~7% (+$0.15)  | −$0.01 (loss) |

Two robust conclusions across the range:

1. **The long tail is value-destructive once you can lose a race.** Most of the 122
   episodes are a few bps; after `p_win` and gas-on-miss their expectation is ≈ 0 or
   **negative**. Submitting them indiscriminately loses money on gas — the same lesson
   as the report's "−$22,812 if you execute every detection," now also true *within*
   the profitable-paper set once frictions apply.
2. **Only the few large dislocations survive**, capturing ~7–48% of their paper value
   depending on latency/competition.

### Aggregate (active hours)

Applying the model episode-by-episode (large ones positive, thin tail ≈ 0/negative),
the **frictions-adjusted active-hours total lands at roughly $1–3/day in the base case**
(vs $17 paper), **~$0 overnight**, so a blended 24h figure is **well under ~$2/day** —
and it is *negative* in the pessimistic case if the thin tail isn't filtered out.

## Recommendation: **NO-GO for live execution now**

The measured edge does **not** survive realistic frictions with any margin:

- Best realistic case is a couple dollars/day, concentrated in a handful of large
  SUI/USDC Cetus↔Turbos dislocations; the rest is gas-negative.
- That is below the cost and risk of running mainnet execution (infra, a funded hot
  wallet, monitoring, MEV/competition that this model treats only parametrically).

This matches the paper report's own conclusion. **Capital is never at risk regardless**
(dry-run + `settle`/`repay` gates), so the downside of *not* shipping execution is nil.

### What would change the verdict (and how we'll know)

- **Drive latency down** (validator-adjacent/co-located submit): the `p_alive`/`p_win`
  terms improve fastest here. Re-run the model with measured latency.
- **Filter to large dislocations only:** raise `min_profit` so the gas-negative thin
  tail is never submitted (the scanner + `RiskGuard` already support this).
- **Collect ground truth:** run the now-real pipeline in **dry-run-only** mode
  (`submit_enabled=false`) and log `RiskGuard::realized_vs_predicted` against live
  dry-runs. If realized capture on the large episodes holds up and the large-episode
  rate rises, revisit with data instead of this model's assumptions.

_Model + parameters: `offchain/src/frictions.rs`. Baseline: `authoritative-pnl-report.md`._
