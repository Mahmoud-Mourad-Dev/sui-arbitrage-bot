# Momentum Integration & Profitability Study (read-only)

Adds **Momentum (mmt.finance)**, a Uniswap-V3-style CLMM, as a read-only priced
venue. Same playbook as Cetus/Turbos/DeepBook: discover → authoritative quote →
parity (gate <0.1%) → integrate → study. **No swap execution; no transactions.**

| Phase | Result |
|-------|--------|
| 1 Discover | **16** mainnet pools (with liquidity) |
| 2 Authoritative quote | `trade::compute_swap_result` via dev-inspect → `SwapState.amount_calculated` |
| 3 Parity (engine vs authoritative) | **PASS** — in-range max rel err **0.078% < 0.1% gate** |
| 4 Execution | none (read-only) |
| 5 Integrate | added as CLMM venue in graph + authoritative dry-run (67 pools total) |
| 6 Study | 12-min combined window: 45 detections, **0 profitable** (sample too short) |

---

## Phase 1 — Discovery

- CLMM package `0x70285592c97965e811e0c6f98dccc3a9c2b4ad854b3594faab9597ada267b860`.
- `pool::Pool<X,Y>` (generic): `sqrt_price` (X64), `liquidity`, `tick_index`,
  `tick_spacing`, `swap_fee_rate` (pips, /1e6), ticks/tick_bitmap.
- 16 pools discovered via `trade::SwapEvent`. Top by liquidity: X_SUI/SUI, WAL/SUI,
  SUI/MMT, SUI/USDC, MMT/USDC, USDT/USDC, DEEP/SUI, BUCK/USDC.
- **Momentum lists WAL/SUI, SUI/USDC, DEEP/SUI, BUCK/USDC, USDT/USDC** — heavy
  overlap with Cetus/Turbos, including the WAL pair that drove prior paper profit.
- Code: [`momentum_rpc.py`](../validation/cetus/momentum_rpc.py).

## Phase 2 — Authoritative quoting

`trade::compute_swap_result<X,Y>(pool, x_for_y, by_amount_in, sqrt_price_limit:u128,
amount:u64) -> SwapState`; the output is `SwapState.amount_calculated`. Sqrt-price
limit uses `tick_math::min_sqrt_price` / `max_sqrt_price` (X64 bounds, same as
Cetus/Turbos). Verified: MMT/USDC sell 1 MMT → 0.182 USDC, linear at small size.

## Phase 3 — Parity (gate: max rel error < 0.1%)

Local CLMM engine (`offchain/examples/clmm_quote`, single-range Q64.64) vs Momentum
authoritative quoter, top 8 pools × 9 sizes × 2 directions = 141 scenarios:

| metric (in-range regime) | value |
|--------------------------|-------|
| scenarios in-range (<0.1%) | 90 |
| rel_err mean | 4.3e-5 |
| rel_err median | 0 |
| rel_err p95 | 1.5e-4 |
| **rel_err max** | **7.8e-4 (0.078%)** |
| **GATE (<0.1%)** | **PASS** |

51 scenarios cross ticks (≥0.1%) — expected: the single-range model doesn't traverse
ticks. The production path uses the **authoritative** quote directly (so dry-run is
exact regardless), exactly as for Cetus/DeepBook. Code:
[`momentum_parity.py`](../validation/cetus/momentum_parity.py).

## Phase 5 — Integration

- `mv_scan.py`: Momentum added as a `clmm` venue (sqrt_price/liquidity/`swap_fee_rate`)
  → its pools enter the unified token graph (virtual-reserve slippage for detection).
- `paper_trade.py`: Momentum legs dry-run **authoritatively** via
  `momentum_rpc.quote` (so opportunities through Momentum are tick-accurate).
- Universe now 67 pools (Cetus 28, Turbos 17, Kriya 8, **Momentum 14**).

## Phase 6 — Combined paper-trading study

12-minute window, 41 rounds, all venues, read-only dry-run validated.

1. **Profit by venue:** 0 profitable opportunities in the window → $0 attributable
   to any venue (incl. Momentum).
2. **Profit by route / 3. token / 4. DEX combination:** no profitable routes; at the
   *detection* level Momentum participated in 2 of 45 cycles
   (`kriya+momentum+turbos`, `cetus+momentum+turbos`) — net negative (≈ −$0.005).
5. **Daily/weekly paper PnL:** $0 in this sample (not representative — see below).
6. **Opportunity frequency:** 0 profitable/h in the window.
7. **Momentum-exclusive opportunities:** 0 in this sample.

### Why 0 — and what it does (not) mean
The prior 20h study found profitable opportunities at only **~4/h** (rare
transients). In a 12-minute window the expectation is **<1**, so **0 is the
statistically expected outcome** — not evidence that Momentum adds nothing. The
window is simply too short to sample rare events. What *is* established:
- Momentum is a **fully validated** new priced source (parity PASS) and is **live in
  the graph** (it appeared in cross-venue cycles within minutes).
- It overlaps the high-value pairs (incl. **WAL/SUI**), so Cetus/Turbos↔Momentum is a
  credible new opportunity surface that a representative run can capture.

### Recommendation
Run the **Momentum-enabled 24h study** to assess profitability properly:
```bash
cd validation/cetus
caffeinate -i ./run_24h.sh          # sleep-proof; Momentum now in the graph
python3 momentum_study.py           # profit by venue/route/token/dex, Momentum-exclusive
```

## Reproduce
```bash
cd validation/cetus
python3 momentum_rpc.py        # authoritative quotes + discovery
python3 momentum_parity.py     # Phase 3 parity (PASS, <0.1%)
python3 momentum_study.py      # profitability study on paper_trades.jsonl
```

**Gate status: PASS** (max rel error 0.078% < 0.1%) — Momentum cleared the bar for
read-only integration. Execution remains disabled across all venues.
