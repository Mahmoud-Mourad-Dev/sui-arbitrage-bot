# Cetus Parity Validation

**Question:** does the Rust CLMM engine (`offchain/src/clmm.rs`) reproduce Cetus's
own on-chain pricing, exactly, across real pools and trade sizes?

**Answer: YES.** 1,080 read-only quotes on **27 live Cetus mainnet pools**;
1,034 gradable (46 excluded as `is_exceed`); **331 of them cross one or more tick
boundaries** (up to **190** crossings in a single swap). For **every** graded
quote the engine output equals Cetus's `calculate_swap_result` **exactly**:

```
abs_error (token units):  mean 0   median 0   p95 0   max 0
rel_error:                mean 0   median 0   p95 0   max 0
FAILURES (> 1 unit and rel >= 1e-6): 0          ACCEPTANCE: PASS
```

This is stronger than the acceptance criterion (≤ 1 unit OR rel < 1e-6): the match
is **bit-identical**. No engine bug was found, so no engine fix was required. The
gate to begin adapter work (P4 in the readiness audit) is **passed for Cetus**.

> Read-only & gasless: every quote is `sui_devInspectTransactionBlock` on
> `pool::calculate_swap_result`. No transaction was ever signed or submitted; no
> funds were used. Network = Sui **mainnet** (chain id `35834a8a`).

---

## Methodology (why these numbers are trustworthy)

- **Authoritative oracle.** Cetus's *own* Move function
  `pool::calculate_swap_result<A,B>(pool, a2b, by_amount_in, amount)` is the ground
  truth — the same math the pool executes. We call it via dev-inspect and BCS-decode
  the `CalculatedSwapResult` (`amount_in, amount_out, fee_amount, fee_rate,
  after_sqrt_price, is_exceed, step_results`). Signatures/layouts were pinned from
  `sui_getNormalizedMoveFunction`/`Struct` — zero guesswork.
- **Atomic state snapshot.** The hard part of differential testing on a live chain
  is that pool state moves between reads. Solved by putting *everything* in ONE
  dev-inspect PTB: `[current_sqrt_price, liquidity, fee_rate, quote₀, quote₁, …]`.
  All quotes and the state fed to the engine come from a single VM snapshot, so the
  engine and Cetus are always compared on identical state. (Tick arrays are fetched
  separately because swaps never change a tick's `liquidity_net`.)
- **Decoder self-checks.** Tick decoding validated by the invariants
  `Σ liquidity_net == 0` (per pool) and `amount_in + fee == requested` on every
  quote. The engine binary (`offchain/examples/clmm_quote`) drives the *production*
  `clmm` module — not a reimplementation.
- **Engine = real code.** The same `clmm::quote_exact_in` shipped and unit-tested
  in the crate produces every engine number here.

## Quote counts (Phases 3 & 5)

| Metric | Value |
|--------|-------|
| Pools tested | 27 (1 skipped: no USD anchor) |
| Total quotes | 1,080 |
| Graded (excl. `is_exceed`) | 1,034 |
| `is_exceed` (trade > pool depth, excluded) | 46 |
| Directions | a→b: 516 · b→a: 518 |
| Sizes per pool | $1 → $50,000 (20-step ladder), both directions |

## Tick-range coverage (Phase 4)

Classified by `n_steps` from the authoritative result (1 = stayed in the current
tick range; >1 = crossed `n_steps−1` initialized ticks):

| Regime | Graded quotes | Max abs error |
|--------|---------------|---------------|
| In-range (1 step) | 703 | 0 |
| Single tick cross (2 steps) | 128 | 0 |
| Many ticks (≥3 steps) | 203 | 0 |
| **Deepest single swap** | **190 tick crossings** | **0** |

Coverage includes the regimes the spec calls out:
- **Concentrated pockets / low spacing:** SCA/SUI (spacing 2), QAI/SUI (2),
  HASUI/SUI (2), US/USDC (2), and the stable pair USDB/BUCK (spacing 1).
- **Low-liquidity / thin ranges:** WSB/SUI, MIU/SUI, TATO/SUI, MMT/SUI, DRF/SUI
  (4–14 initialized ticks) — these cross many ticks even at small sizes
  (e.g. TATO/SUI@40000: 40/40 samples cross-tick).
- **Deep books crossing many ticks at size:** USDC/SUI, DEEP/SUI, CETUS/SUI,
  NS/SUI (hundreds of ticks; large trades sweep dozens–hundreds).

## Pools tested (Phase 1 decode targets)

All pool core state (`current_sqrt_price`, `liquidity`, `current_tick_index`,
`fee_rate`, `tick_spacing`) and full tick tables (`index`, `sqrt_price`,
`liquidity_net` via `pool::fetch_ticks`) were decoded read-only.

| # | pool id | pair | fee | spacing | ticks | samples | cross-tick |
|---|---------|------|-----|---------|-------|---------|-----------|
| 1 | `0xe01243f3…cbd2` | DEEP/SUI | 2500 | 60 | 457 | 40 | 8 |
| 2 | `0x51e883ba…d2ab` | USDC/SUI | 500 | 10 | 665 | 40 | 10 |
| 3 | `0x213e8204…381c` | QAI/SUI | 100 | 2 | 4 | 40 | 0 |
| 4 | `0xf4238fa5…6e4c` | WAL/SUI | 500 | 10 | 140 | 40 | 13 |
| 5 | `0x871d8a22…96bc` | HASUI/SUI | 100 | 2 | 389 | 40 | 0 |
| 6 | `0x9661cca0…92e9` | SCA/SUI | 100 | 2 | 17 | 40 | 11 |
| 7 | `0x2e041f3f…bded` | CETUS/SUI | 2500 | 60 | 554 | 40 | 12 |
| 8 | `0x9e59de50…ac88` | USDC/ETH | 2500 | 60 | 238 | 40 | 7 |
| 9 | `0xb8d7d9e6…0105` | USDC/SUI | 2500 | 60 | 590 | 40 | 0 |
| 10 | `0xdcd97bb5…e19b` | JACKSON/SUI | 2500 | 60 | 12 | 40 | 19 |
| 11 | `0x36364c1f…f741` | MMT/SUI | 10000 | 200 | 7 | 40 | 3 |
| 12 | `0xded83f5b…0874` | WSB/SUI | 2500 | 60 | 4 | 40 | 20 |
| 13 | `0x6c545e78…f535` | CERT/SUI | 100 | 2 | 186 | 40 | 11 |
| 14 | `0xcd46e4a5…e882` | TATO/SUI | 20000 | 220 | 4 | 40 | 28 |
| 15 | `0x76cab5e8…6aeb` | TATO/SUI | 40000 | 260 | 14 | 40 | 40 |
| 16 | `0x4edb54ba…a421` | MIU/SUI | 2500 | 60 | 4 | 40 | 17 |
| 17 | `0x763f63cb…5520` | NS/SUI | 2500 | 60 | 227 | 40 | 18 |
| 18 | `0x4f665396…75d5` | USDC/WAL | 500 | 10 | 83 | 40 | 13 |
| 19 | `0x59cf0d33…bf6c` | BUCK/SUI | 500 | 10 | 88 | 40 | 6 |
| 20 | `0x7a0abae4…c973` | USDB/BUCK | 10 | 1 | 7 | 40 | 0 |
| 21 | `0xb3c8fbd5…6e76` | MAGMA/SUI | 10000 | 200 | 14 | 40 | 4 |
| 22 | `0xd978d331…9126` | DEEP/SUI | 500 | 10 | 118 | 40 | 22 |
| 23 | `0x968f1f73…e7bb` | US/USDC | 100 | 2 | 13 | 40 | 5 |
| 24 | `0x6daa2180…fbf0` | DRF/SUI | 10000 | 200 | 6 | 40 | 20 |
| 25 | `0xa2f4e24d…15f0` | DEEP/USDC | 500 | 10 | 27 | 40 | 10 |
| 26 | `0x711f5d37…3359` | USDC/HAEDAL | 2500 | 60 | 95 | 40 | 10 |
| 27 | `0xc4c09f20…c977` | FAITH/SUI | 10000 | 200 | 31 | 40 | 24 |

(cross-tick column = graded cross-tick samples; full ids in `validation/cetus/pools_config.json`.)

## Error statistics (Phase 5)

Over all 1,034 graded quotes:

| Statistic | abs_error (token units) | rel_error |
|-----------|-------------------------|-----------|
| mean | 0 | 0 |
| median | 0 | 0 |
| p95 | 0 | 0 |
| max | 0 | 0 |

## Discovered bugs

- **CLMM engine: none.** Zero quotes deviated from Cetus, in-range or cross-tick,
  so the X64 `SqrtPriceMath`, the tick-crossing loop, the `liquidity_net`
  application direction, and the fee model are all confirmed correct against the
  live protocol. No engine change was made for this validation.
- **Harness (not the engine), fixed during bring-up:**
  1. *State drift on busy pools.* The first harness read pool state and quotes in
     separate RPC calls and guarded by object version; the two busiest pools
     (DEEP/SUI, USDC/SUI) changed state every few seconds and never produced a
     stable window. **Fix:** fold state + all quotes into a single dev-inspect PTB
     (one atomic snapshot). This is the methodology above.
  2. *Empty-batch edge.* `run_engine` mis-handled a zero-sample pool. Fixed with a
     guard.

## Caveats / scope

- `is_exceed` quotes (46) — where the requested size exceeds available depth to the
  price limit — are reported separately, not graded, since the cap behavior is a
  boundary policy rather than a pricing comparison.
- One pool (HASUI/HAEDAL) was skipped: neither side is SUI/stable, so no clean USD
  anchor for sizing (not a pricing limitation).
- Validated venue: **Cetus**. Turbos and Kriya-CLMM reuse the same engine but must
  pass their own parity run against their on-chain quoters before adapters ship.

## Reproduce

```bash
cd offchain && cargo build --release --example clmm_quote
cd ../validation/cetus
python3 discover.py        # find live pools + decimals + SUI/USD anchor
python3 parity.py          # full differential run -> parity_results.json
```

Framework (read-only): `validation/cetus/cetus_rpc.py` (state/tick decode + quoter),
`discover.py` (pool discovery), `parity.py` (differential test + stats).
