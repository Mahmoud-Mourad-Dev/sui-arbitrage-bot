# Authoritative-Pricing Paper-Trading Report

Multi-venue dry-run study with **all CLMM legs priced by each venue's own on-chain
quoter** (Cetus, Turbos, Momentum, Bluefin via `*_rpc`; Kriya CP exact). No engine
single-range estimate anywhere — this is the corrected PnL after the Turbos parity
fix. Read-only `devInspect` throughout; no execution.

## Two windows (profit is strongly time-of-day dependent)

| Window | Span | Detections | Profitable | Realistic P&L (dedup) | /day |
|--------|------|-----------|-----------|----------------------|------|
| Overnight (00:06–06:21) | 6.26 h | 3,892 | 6 | $0.0121 | ~$0.05 |
| **Active hours (06:49–18:12)** | **11.40 h** | **8,751** | **145 (12.7/h)** | **$8.14** (122 episodes) | **~$17** |

The overnight market is efficient (near-zero opportunity); during active trading
hours real cross-venue dislocations appear. Both runs are 100% authoritatively
priced (8,751/8,751 and 3,892/3,892), so this is a genuine market effect, not a
pricing artifact.

## Active-hours run — full breakdown (11.40 h)

- Detections **8,751**; profitable (dry-run net>0) **145** (~12.7/h).
- **Raw paper P&L $10.22** (~$21.5/day) · **deduped to 122 episodes → $8.14**
  (~$17/day) — the realistic figure (one capture per persistent dislocation).
- **If you executed every detection: −$22,812.** The dry-run gate is essential —
  the overwhelming majority of edge>0 candidates are net losers after authoritative
  pricing + gas.

### Profit by route (top)
| route | n | total $ | mean $ |
|-------|---|---------|--------|
| USDC→SUI→USDC | 5 | 3.09 | 0.62 |
| USDC→SUI→DEEP→USDC | 13 | 1.20 | 0.09 |
| USDC→SUI→USDSUI→USDC | 6 | 0.99 | 0.16 |
| USDC→SUI→BUCK→USDC | 12 | 0.74 | 0.06 |
| USDC→SUI→WAL→USDC | 6 | 0.38 | 0.06 |

**Concentration:** top route = 30%, top 3 = 52% of all paper profit.

### Profit by DEX combination (top)
| venues | n | total $ | mean $ |
|--------|---|---------|--------|
| cetus×1 + turbos×1 | 2 | 2.17 | 1.09 |
| cetus×3 | 9 | 1.29 | 0.14 |
| bluefin×1 + cetus×2 | 15 | 0.93 | 0.06 |
| bluefin×1 + turbos×1 | 3 | 0.89 | 0.30 |
| bluefin×1 + cetus×1 + turbos×1 | 12 | 0.85 | 0.07 |

Cross-venue **Cetus↔Turbos↔Bluefin** dominates. **Turbos legs are now
authoritatively priced and contribute real profit** — i.e. the prior "94% via
Turbos (engine-estimated)" is, under honest pricing, genuine for the major-pair
cross-venue cycles (no longer a single-range artifact).

### Cumulative PnL by token (involvement)
SUI $10.06 · USDC $10.01 · DEEP $2.32 · USDSUI $2.09 · WAL $1.18 · BUCK $1.18 ·
SCA $0.97 · LBTC $0.40 · USDT $0.38 · MMT $0.27. SUI/USDC are in nearly every
profitable cycle.

### Top opportunity
`USDC→SUI→USDC` (Cetus↔Turbos), $1,000 size, 29.4 bps → **+$2.14** net.

### Largest would-be losers (correctly excluded by the gate)
`USDC→…→BUCK→USDB→USDC` via Momentum: net **−$50 to −$100** (a USDB leg quotes
~0 → total loss of input). The dry-run prices these as catastrophic and they are
**not** executed — exactly the gate's job. (Flag: the Momentum USDB/USDC leg looks
broken/empty; exclude that pool.)

## Honest caveats
1. **Realistic capturable P&L ≈ $17/day** (deduped), not the raw $21.5/day.
2. **Time-of-day dependent** — overnight is ~$0/day. A true 24h average lands
   between the two windows.
3. **Not modeled:** MEV/competition (others arb the same dislocations), latency
   (the edge may close before you land), price impact of actually capturing,
   and gas beyond the flat estimate. Real captured profit will be **lower**.
4. Neither window reached a full 24h (Mac slept). Figures are per-hour rates over
   11.4 h and 6.26 h respectively.

## Verdict
Under fully authoritative pricing, on-chain CLMM cross-venue arbitrage on Sui shows
**small but non-zero** paper profit during active hours (~$17/day realistic,
concentrated in SUI/USDC Cetus↔Turbos↔Bluefin), and ~nil overnight. This is a real
signal but **marginal** and below what survives live frictions — so it does **not**
yet justify execution work. The right next step before any execution is to model the
live frictions (latency/competition) against this paper baseline.

_Source data: `validation/cetus/paper_trades.jsonl` (active run, gitignored);
analysis via `analyze_pnl.py` / `momentum_study.py`._
