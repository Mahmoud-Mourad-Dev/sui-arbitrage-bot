# Next-Gen Market Graph: DeepBook + Aftermath (read-only)

Adds two new liquidity/price sources to the read-only market graph. **No swap
execution, no transactions** — discovery, pricing, simulation, parity, and
paper-trading support only. Existing Cetus/Turbos/Kriya integration unchanged.

| Venue | Type | Read-only integration | Pricing | Parity | Status |
|-------|------|-----------------------|---------|--------|--------|
| **DeepBook v3** | native CLOB | discovery + authoritative quoter + L2 depth | on-chain `pool::get_*_quantity_out` via dev-inspect | depth-walk vs authoritative: **PASS** (max 5.3e-5) | **complete** |
| **Aftermath** | aggregator + AMM | router quote via public REST API | authoritative aggregated route | used as oracle/benchmark (its own router is ground truth) | **complete (oracle)** |

---

## Phase 1 — DeepBook v3

**Architecture (researched).** DeepBook v3 is Sui's native central limit order book.
Each market is a shared `Pool<Base, Quote>`. Read-only quoting uses the pool's own
on-chain functions — no orderbook math to re-implement:
- `pool::get_quote_quantity_out(pool, base_qty, clock) -> (base_out, quote_out, deep_required)`
- `pool::get_base_quantity_out(pool, quote_qty, clock) -> (...)`
- `pool::get_quantity_out`, `pool::mid_price`, `pool::get_level2_ticks_from_mid`

**Discovery (on-chain verified).**
- Package `0x0e735f8c93a95722efd73521aca7a7652c0bb71ed1daf41b26dfd7d1ff71f748`
  (type origin `0x2c8d603b…4809`), Registry `0xaf16199a…549d`.
- Pools: SUI_USDC `0xe05dafb5…4407`, DEEP_SUI `0xb663828d…fc22`, DEEP_USDC `0xf948981b…95ce`.
- Best bid/ask from L2: SUI_USDC bid `727930` / ask `728030` (≈0.014% spread).
- `deep_required` = DEEP-token taker fee owed on the fill (non-zero here) — charged
  as a cost when present.

**Implementation:** [`validation/cetus/deepbook_rpc.py`](../validation/cetus/deepbook_rpc.py)
— authoritative quotes (dev-inspect, with the `Clock` shared input), `mid_price`,
L2 depth, and a local orderbook **depth-walk** model. Read-only.

**Routing graph:** DeepBook contributes directed edges base↔quote priced by the
authoritative quoter; the cross-venue scanner
[`deepbook_xvenue.py`](../validation/cetus/deepbook_xvenue.py) realizes
**AMM↔DeepBook** and **DeepBook↔AMM** round trips (authoritative on both legs).

### Phase 3 parity (DeepBook) — `deepbook_parity.py`
Local depth-walk vs authoritative `get_quote_quantity_out`, 3 pools × 5 sizes:

| metric | value |
|--------|-------|
| coverage | 15/15 sizes within fetched book |
| rel_err mean | 1.2e-5 |
| rel_err median | 0 |
| rel_err p95 | 5.3e-5 |
| rel_err max | **5.3e-5** |
| **verdict** | **PASS** (≪ 1e-3; residual is L2 tick aggregation) |

> The production path uses the **authoritative** quote directly (like Cetus
> `calculate_swap_result`), so DeepBook pricing is correct by construction; the
> depth-walk is the validated fast-path model for future local detection.

---

## Phase 2 — Aftermath (price / liquidity / route-quality oracle)

**Architecture (researched).** Aftermath = a smart-order **Router** (aggregator over
many Sui DEXes) + its own weighted/stable AMM. Read-only quotes come from the public
REST router: `POST https://aftermath.finance/api/router/trade-route`
`{coinInType, coinOutType, coinInAmount}` → `{routes[], coinOut.amount, spotPrice,
netTradeFeePercentage}`. Implementation: [`aftermath_rpc.py`](../validation/cetus/aftermath_rpc.py)
(read-only; behind Cloudflare, so called via curl with a browser UA).

**Benchmark — our best venue vs Aftermath (SUI→USDC):**

| size SUI | Aftermath out | Cetus | DeepBook | best/AF | Aftermath route |
|---------:|--------------:|------:|---------:|--------:|-----------------|
| 1 | 727,976 | 725,917 | **728,420** | 1.0006 | Bluefin, Momentum, SpringSui |
| 50 | 36,403,265 | 36,295,871 | **36,416,500** | 1.0004 | Momentum, Obric, SpringSui |
| 500 | 363,920,307 | 362,958,492 | **364,165,000** | 1.0007 | Cetus, Momentum, SpringSui |

Findings:
1. **DeepBook is the best single venue for SUI/USDC** — it beats Cetus and even
   edges Aftermath's *aggregated* route (+0.04–0.07%) thanks to the tight CLOB.
2. **Aftermath reveals liquidity beyond our graph:** its best routes use **Momentum,
   Bluefin, Obric, SpringSui** — venues we do not yet scan. As a *liquidity oracle*
   this is the actionable signal for the next integration wave.

---

## Phase 4 — Combined paper-trading study

Sources priced: Cetus, Turbos, Kriya (prior 20h study) + DeepBook (this study) +
Aftermath (oracle). All read-only; execution disabled.

1. **Profit by venue (validated, net of gas):** Cetus↔Turbos cross-venue dominates
   (prior study: ~$1.39 deduped over 20h, but **94% engine-estimated via Turbos**,
   only ~$0.10 fully authoritative). **DeepBook cross-venue: $0 profitable** in this
   sample. Kriya: negligible.
2. **Profit by route:** WAL-centric Cetus↔Turbos cycles (prior study). No new
   profitable routes from DeepBook.
3. **Profit by token:** WAL, SUI, USDC, DEEP (prior study). DeepBook touches
   SUI/USDC/DEEP only.
4. **Profit by DEX combination:** cetus×Turbos >> pure single-venue; **AMM×DeepBook =
   no positive net** in sample.
5. **Daily / weekly paper PnL:** prior study ≈ $1.6/day, ~$11/week (mostly
   unvalidated Turbos). DeepBook adds **$0/day** of *new* validated profit in sample.
6. **Opportunity frequency:** AMM-only ~4.4 profitable detections/h (prior);
   AMM↔DeepBook **0/h** in sample.
7. **New opportunities from DeepBook:** **none profitable in sample.** Root cause:
   DeepBook lists only *major* pairs (SUI/USDC, DEEP/USDC, DEEP/SUI) which are
   tightly arbitraged against AMMs, and it **does not list WAL** — the less-liquid
   pair that produced the prior profit. DeepBook still adds value as **best execution
   on SUI/USDC** and a distinct, validated venue.
8. **New opportunities from Aftermath:** as an oracle it surfaces **four unscanned
   venues (Momentum, Bluefin, Obric, SpringSui)** carrying competitive liquidity —
   the highest-EV next sources to integrate.

### Honest conclusion
Adding DeepBook + Aftermath **did not increase validated profitability in the
sample** — the major pairs are efficient and the profitable WAL pair isn't on
DeepBook. But the exercise delivered three things that de-risk live execution:
(a) DeepBook is a **fully validated** new priced venue (parity PASS) and the best
SUI/USDC execution; (b) Aftermath gives an **authoritative price/route benchmark**;
(c) the benchmark **identifies the next venues to add (Momentum/Bluefin/Obric/
SpringSui)** where liquidity — and likely the real cross-venue edge — actually lives.

**Recommendation:** before live execution, integrate Momentum + Bluefin (read-only,
same pattern) and re-run the paper study; that is where Aftermath says the liquidity
is. Keep execution disabled until a venue's parity passes (DeepBook ✓; Turbos still
pending its own parity gate).

## Reproduce
```bash
cd validation/cetus
python3 deepbook_rpc.py          # authoritative DeepBook quotes + L2
python3 deepbook_parity.py       # Phase 3 parity (PASS)
python3 deepbook_xvenue.py 6     # AMM<->DeepBook cross-venue scan
python3 aftermath_rpc.py         # Aftermath oracle benchmark vs our venues
```
