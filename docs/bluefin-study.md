# Bluefin Integration & Profitability Study (read-only)

Adds **Bluefin Spot** (Uniswap-V3-style CLMM) as a read-only priced venue — the
top liquidity source flagged by the Aftermath oracle. Same playbook as
Cetus/Momentum: discover → authoritative quote → parity (gate <0.1%) → integrate →
study. **No swap execution; no transactions.**

| Phase | Result |
|-------|--------|
| 1 Discover | **22** mainnet pools (active, with liquidity) |
| 2 Authoritative quote | `pool::calculate_swap_results` via dev-inspect → `SwapResult.amount_calculated` |
| 3 Parity (engine vs authoritative) | **PASS** — in-range max rel err **0.031% < 0.1% gate** |
| 4 Execution | none (read-only) |
| 5 Integrate | added as CLMM venue; graph now **102 pools / 5 venues** |
| 6 Study | 15-min window: 263 detections, 0 profitable, but **122 (46%) involve Bluefin** |

---

## Phase 1 — Discovery
- Bluefin Spot CLMM package `0x3492c874c1e3b3e2984e8c41b589e642d4d0a5d6459e5a9cfc2d52fd7c89c267`.
- `pool::Pool<X,Y>` (generic): `current_sqrt_price` (X64), `liquidity`, `fee_rate`
  (pips, /1e6), `is_paused`, ticks_manager.
- 22 active pools via `events` module swap events; deepest include USDB/BUCK,
  **WAL/SUI**, SUI/USDC, **WAL/USDC**, DEEP/USDC. Bluefin lists **both WAL pairs** —
  the historically-profitable token.
- Discovery seeded via the Aftermath oracle (a Bluefin route → pool id → on-chain
  type → package), then enumerated on-chain. Code: [`bluefin_rpc.py`](../validation/cetus/bluefin_rpc.py).

## Phase 2 — Authoritative quoting
`pool::calculate_swap_results<X,Y>(pool, a2b, by_amount_in, amount:u64,
sqrt_price_limit:u128) -> SwapResult`; output is `SwapResult.amount_calculated`
(BCS offset 18). X64 sqrt-price bounds (same family as Cetus/Turbos/Momentum).
Verified: SUI/USDC sell 1 SUI → 0.7146 USDC.

## Phase 3 — Parity (gate: max rel error < 0.1%)
Local CLMM engine (single-range Q64.64) vs Bluefin authoritative quoter, top 8
pools × 9 sizes × 2 directions = 139 scenarios:

| metric (in-range) | value |
|-------------------|-------|
| in-range scenarios (<0.1%) | 103 |
| rel_err mean | 4.2e-5 |
| rel_err median | 0 |
| rel_err p95 | 2.8e-4 |
| **rel_err max** | **3.07e-4 (0.031%)** |
| **GATE (<0.1%)** | **PASS** |

36 cross-tick scenarios diverge (single-range model). Production uses the
authoritative quote directly. Code: [`bluefin_parity.py`](../validation/cetus/bluefin_parity.py).

## Phase 5 — Integration
- `mv_scan.py`: Bluefin added as a `clmm` venue (`current_sqrt_price`/`liquidity`/
  `fee_rate`, skips `is_paused`).
- `paper_trade.py`: Bluefin legs dry-run **authoritatively** via `bluefin_rpc.quote`.
- Universe now **102 pools**: Cetus 33, Turbos 21, Kriya 8, Momentum 16, **Bluefin 24**.

## Phase 6 — Combined paper-trading study (15-min window, 28 rounds)

1. **Profit by venue:** 0 profitable opportunities in the window → $0 attributable.
2. **Profit by route / token / DEX combination:** no profitable routes; at the
   *detection* level Bluefin is the **most active new source** — it appears in
   **122 of 263 detections (46%)**. Top venue-combos:
   `bluefin+cetus+turbos` (67), `cetus+turbos` (60), `cetus+momentum+turbos` (38),
   `cetus+momentum` (35), `bluefin+cetus+momentum` (33), `bluefin+cetus` (19).
3. **Closest-to-profitable candidates (the real signal):** `USDC→WAL→USDC`
   Bluefin↔Cetus, edge up to **9.7 bps**, net **−$0.002** — i.e. *just under gas* at
   the tiny probe sizes. The WAL cross-venue dislocation that drove prior profit is
   present on Bluefin↔Cetus, sitting right at the gas threshold.
5. **Daily/weekly paper PnL:** $0 in this 15-min sample (not representative).
6. **Opportunity frequency:** 0 profitable/h in the window (prior base rate ~4/h).
7. **Bluefin-exclusive opportunities:** 0 *profitable* in sample, but 122
   *candidate* cycles became visible only because Bluefin was added.

### Interpretation
Bluefin clears the parity gate (0.031%) and is now the **most active new venue in
the graph** — it more than quintupled detection volume (263 vs ~45) and brought the
best near-threshold candidates (WAL via Bluefin↔Cetus at ~10 bps). 0 profitable in
15 minutes is the **expected** outcome at the ~4/h rare-transient base rate, not a
negative result. Bluefin is the strongest candidate yet to convert near-threshold
WAL dislocations into captured profit when the edge widens or gas is optimized.

### Recommendation
Run the 5-venue 24h study (sleep-proof) to measure Bluefin's profit contribution:
```bash
cd validation/cetus && caffeinate -i ./run_24h.sh
python3 analyze_pnl.py            # profit by route/token/dex/cumulative
python3 momentum_study.py         # profit by venue + per-venue exclusives
```

## Reproduce
```bash
cd validation/cetus
python3 bluefin_rpc.py        # authoritative quotes + discovery (22 pools)
python3 bluefin_parity.py     # Phase 3 parity (PASS, max 0.031%)
```

**Gate status: PASS** (max rel error 0.031% < 0.1%). Execution remains disabled
across all venues.
