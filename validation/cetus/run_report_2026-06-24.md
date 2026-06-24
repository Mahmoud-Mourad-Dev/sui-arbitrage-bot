# 10–12h Authoritative Paper-Trading Report (2026-06-24 06:54:36 → 2026-06-24 17:09:35)

## Run Summary
- start: 2026-06-24 06:54:36  ·  stop: 2026-06-24 17:09:35  ·  duration: 10.25 h
- rounds completed: 1829  ·  pools: 86  ·  venues: 5 (cetus,turbos,kriya,momentum,bluefin)
- records (detections): 3602  ·  authoritative dry-runs: 3602

## Opportunity Statistics
- detections: 3602  (351.4/h)
- profitable (sim_net>0): 20  (1.95/h)
- would_execute: 20  (rate 0.56% of detections)

## Profitability (USD, simulated authoritative net)
- gross paper P&L: $0.4678  ·  per day: $1.10
- deduplicated P&L: $0.4678 (20 episodes)  ·  per day: $1.10
- avg $0.0234 · median $0.0075 · p95 $0.2103 · max $0.2103 · min $0.0005

## Route Analysis (top 20 by profit)
- $0.2103 (1x)  SUI -> USDC -> MMT -> SUI
- $0.0781 (1x)  SUI -> DEEP -> USDC -> MMT -> SUI
- $0.0703 (2x)  SUI -> COIN -> USDC -> SUI
- $0.0249 (1x)  SUI -> WAL -> USDC -> MMT -> SUI
- $0.0137 (2x)  SUI -> BUCK -> SUI
- $0.0122 (1x)  SUI -> COIN -> USDC -> BUCK -> SUI
- $0.0105 (2x)  SUI -> BUCK -> USDC -> SUI
- $0.0097 (1x)  SUI -> COIN -> USDC -> SCA -> SUI
- $0.0087 (1x)  SUI -> WAL -> USDC -> SUI
- $0.0075 (1x)  SUI -> USDC -> COIN -> SUI
- $0.0059 (1x)  SUI -> COIN -> USDC -> STSUI -> SUI
- $0.0054 (1x)  SUI -> USDC -> SCA -> SUI
- $0.0036 (1x)  SUI -> WAL -> USDC -> BUCK -> SUI
- $0.0029 (1x)  SUI -> USDC -> BUCK -> SUI
- $0.0028 (1x)  SUI -> USDC -> STSUI -> SUI
- $0.0008 (1x)  SUI -> WAL -> USDC -> COIN -> SUI
- $0.0005 (1x)  SUI -> USDC -> DEEP -> WAL -> SUI
- route concentration: top route = 45.0%  ·  top3 = 76.7%

## DEX Analysis (profit by venue combination)
- $0.2103 (1x)  cetusx1 + momentumx2
- $0.0781 (1x)  cetusx2 + momentumx2
- $0.0623 (1x)  kriyax1 + momentumx1 + turbosx1
- $0.0249 (1x)  cetusx1 + momentumx3
- $0.0155 (2x)  cetusx1 + kriyax1 + momentumx1
- $0.0137 (2x)  cetusx1 + kriyax1
- $0.0122 (1x)  cetusx2 + kriyax1 + momentumx1
- $0.0105 (2x)  bluefinx1 + cetusx1 + kriyax1 + momentumx1
- $0.0089 (2x)  cetusx2 + kriyax1
- $0.0087 (1x)  cetusx1 + turbosx2
- $0.0059 (1x)  bluefinx1 + kriyax1 + momentumx2
- $0.0054 (1x)  bluefinx1 + cetusx2

## Token Analysis
profit by token (involvement): SUI $0.47, USDC $0.45, MMT $0.31, COIN $0.11, DEEP $0.08, BUCK $0.04, WAL $0.04, SCA $0.02, STSUI $0.01
top profitable pairs: SUI/USDC $0.319, MMT/USDC $0.313, MMT/SUI $0.313, COIN/SUI $0.106, COIN/USDC $0.106, DEEP/USDC $0.079, DEEP/SUI $0.078, BUCK/SUI $0.057
most frequent pairs: SUI/USDC (11), BUCK/SUI (9), COIN/SUI (7), COIN/USDC (7), BUCK/USDC (5), SUI/WAL (5), USDC/WAL (4), MMT/USDC (3)

## Opportunity Distribution (profit >)
- > $0.10: 1
- > $0.25: 0
- > $0.50: 0
- > $1.00: 0
- > $2.00: 0
- > $5.00: 0

## Execution Readiness (arb friction model: lat=800ms, comp=3, gas=$0.03)
- estimated capture rate: -53.4%
- net after frictions (window): $-0.2500
- expected daily net: $-0.59

## Top 20 opportunities
- $0.2103  39.2bps  SUI -> USDC -> MMT -> SUI  [cetusx1 + momentumx2]
- $0.0781  38.4bps  SUI -> DEEP -> USDC -> MMT -> SUI  [cetusx2 + momentumx2]
- $0.0623  17.7bps  SUI -> COIN -> USDC -> SUI  [kriyax1 + momentumx1 + turbosx1]
- $0.0249  40.0bps  SUI -> WAL -> USDC -> MMT -> SUI  [cetusx1 + momentumx3]
- $0.0122  13.4bps  SUI -> COIN -> USDC -> BUCK -> SUI  [cetusx2 + kriyax1 + momentumx1]
- $0.0116  19.9bps  SUI -> BUCK -> SUI  [cetusx1 + kriyax1]
- $0.0097  35.1bps  SUI -> COIN -> USDC -> SCA -> SUI  [bluefinx1 + cetusx1 + kriyax1 + momentumx1]
- $0.0087  27.4bps  SUI -> WAL -> USDC -> SUI  [cetusx1 + turbosx2]
- $0.0080  3.8bps  SUI -> COIN -> USDC -> SUI  [cetusx1 + kriyax1 + momentumx1]
- $0.0075  2.0bps  SUI -> USDC -> COIN -> SUI  [cetusx1 + kriyax1 + momentumx1]
- $0.0059  17.5bps  SUI -> BUCK -> USDC -> SUI  [cetusx2 + kriyax1]
- $0.0059  9.7bps  SUI -> COIN -> USDC -> STSUI -> SUI  [bluefinx1 + kriyax1 + momentumx2]
- $0.0054  21.1bps  SUI -> USDC -> SCA -> SUI  [bluefinx1 + cetusx2]
- $0.0045  20.1bps  SUI -> BUCK -> USDC -> SUI  [cetusx1 + kriyax1 + turbosx1]
- $0.0036  24.3bps  SUI -> WAL -> USDC -> BUCK -> SUI  [cetusx3 + turbosx1]
- $0.0029  24.2bps  SUI -> USDC -> BUCK -> SUI  [cetusx2 + kriyax1]
- $0.0028  19.9bps  SUI -> USDC -> STSUI -> SUI  [bluefinx1 + cetusx1 + momentumx1]
- $0.0022  17.7bps  SUI -> BUCK -> SUI  [cetusx1 + kriyax1]
- $0.0008  18.3bps  SUI -> WAL -> USDC -> COIN -> SUI  [bluefinx1 + cetusx1 + kriyax1 + momentumx1]
- $0.0005  4.1bps  SUI -> USDC -> DEEP -> WAL -> SUI  [bluefinx1 + cetusx3]
## Comparison vs previous authoritative runs
Source for prior numbers: docs/authoritative-pnl-report.md (raw jsonl archived as paper_trades_pre12h_20260624_0653.jsonl).

| metric | THIS run (10.25h) | prev active-hours (11.4h) | prev overnight (6.26h) |
|---|---|---|---|
| detections | 3,602 (351/h) | 8,751 (768/h) | 3,892 (622/h) |
| profitable | 20 (1.95/h) | 145 (12.7/h) | 6 (~1/h) |
| gross paper P&L | $0.47 (~$1.10/day) | $10.22 (~$21.5/day) | ~$0 |
| dedup P&L | $0.47 (~$1.10/day) | $8.14 (~$17/day) | $0.0121 (~$0.05/day) |
| profit / hour | $0.046 | $0.714 | ~$0.002 |
| route concentration (top / top3) | 45% / 77% | 30% / 52% | n/a |

Differences: this window had ~half the detection rate (86 pools vs more, + a calm market — SUI flat $0.68–0.70 all day), ~6.5x fewer profitable opportunities, ~15x lower profit/hour, and HIGHER concentration (one MMT route = 45%). It sits between the prior active and overnight windows, much closer to overnight — i.e. a low-volatility day.

## Execution Readiness — verdict: NO-GO
- Gross ~$1.10/day; **net after the arb friction model is NEGATIVE (~-$0.59/day)** — the 20 episodes are tiny (median $0.0075, only ONE > $0.10), so expected gas-on-miss exceeds capture.
- This reinforces the standing arb verdict: pure cross-venue arb does not clear live frictions. A calm window makes it worse.

## Top findings
1. Opportunity flow is volatility-driven: a flat-SUI day yielded ~1/15th the profit/hour of the prior active window.
2. The edge is thin and concentrated: 1 route = 45%, top 3 = 77%; only one opportunity all day exceeded $0.10.
3. Net-of-frictions is negative — the long tail of sub-$0.01 "profitable" detections is gas-negative.
4. Pricing is honest: 3,602/3,602 authoritative dry-runs; no model blow-ups drove the result.

## Recommended next actions
1. Do NOT enable arb submission (stays NO-GO). Keep submit_enabled=false.
2. Pivot measurement to LIQUIDATIONS (fat tail) — run the liquidation paper loop over a VOLATILE multi-day window; that is the only path with plausibly GO economics.
3. If pursuing arb at all: raise min_profit to cut the gas-negative tail, and only run during high-volatility hours (skip calm days entirely).
4. Re-run this arb paper on a high-volatility day to bound the upside; one calm window is not the worst case but confirms the floor.
