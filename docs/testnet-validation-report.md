# Testnet Validation Report

> ## ⚠️ SCOPE — what this validated (read first)
> This run validated **only the in-package reference AMM** (`arbitrage_system::amm_v2`
> + the `validation_coins` test tokens) end-to-end: off-chain `x*y=k` sim == on-chain
> execution through `executor::begin/settle`. It is strong evidence for **the profit
> gate + the V2 math + the PTB round-trip**, and nothing more.
>
> It does **NOT** validate the mainnet-facing live path: the real **Cetus / Turbos**
> swap adapters, the CLMM authoritative quoter, live ingestion, dry-run → submit, the
> **liquidation** flow, or any **flash-loan** (Scallop) interaction. Those are **not yet
> validated on-chain** — that work is tracked in **[testnet-runbook.md](testnet-runbook.md)**
> and is pending a published package + a funded testnet key. Do **not** cite the numbers
> below as evidence the live/adapter code works.

**System:** `arbitrage_system` (Sui Move) + `arb-scanner` (Rust) — **reference AMM only**
**Network:** Sui Testnet · **Date:** 2026-06-21
**Toolchain:** `sui 1.73.1`, `rustc/cargo 1.96.0`
**Result:** ✅ PASS *(reference-AMM scope)* — off-chain sim matched on-chain execution
to the unit (**error = 0.000%**, goal < 1%); all four failure modes behaved as designed.
**This says nothing about the Cetus/Turbos adapters or the live path.**

---

## 0. Environment verification (Phase 1)

| Check | Result |
|-------|--------|
| `sui --version` | 1.73.1-homebrew |
| `rustc --version` | 1.96.0 |
| `cargo --version` | 1.96.0 |
| `sui move build` | ✅ |
| `sui move test` | ✅ 8/8 |
| `cargo test` | ✅ 7/7 |
| `cargo clippy --all-targets -- -D warnings` | ✅ clean |
| `cargo fmt --check` | ✅ clean |

## 1. Deployment details (Phase 2)

Full artifact list: [deployment-testnet.md](deployment-testnet.md).

| Item | Value |
|------|-------|
| Deployer | `0x1fb0…dd08` |
| Faucet (1 SUI) | `BUzTWsUs2zMkheqKVgn2raFdQSFxtBoKVDQ6iTf9de1A` |
| `arbitrage_system` package | `0x5de0227d4455506b87ad26af9cb615a6d51f50668091f3066172782bab4521ea` |
| publish digest | `DbuJvKXJUdCxgZYCbRcd5ekxqxqZQts29sN8iMcPTt1Q` |
| AdminCap | `0x0118…a896` |
| `validation_coins` package | `0x60e43310fb4817f447afd819ba95beb6afea3f90489ed42f0f284b1cd545d6bc` |
| publish digest | `GJZCeoyaTeVHVvpBJRJ8ot39M98nxRyMkNQNEoQPB88N` |

## 2. Pool configuration (Phase 3)

Created atomically in one PTB · digest `EtqSXftCWMC69aNQoQgFsQXZzCgvAp2A7wXgTot9L5Rc`.

| Pool | Object ID | Reserves | Implied price |
|------|-----------|----------|---------------|
| A/B | `0xacb6…943a` | 1e12 A / 1e12 B | 1 A = 1 B |
| B/C | `0xad2b…4174` | 1e12 B / 1e12 C | 1 B = 1 C |
| C/A | `0x8a4f…be77` | 1e12 C / 2e12 A | **1 C = 2 A** |

Reserves read back from chain after creation — exact match to intent. The C/A
dislocation makes A→B→C→A profitable. Fee = 30 bps per hop.

## 3. PTB structure (Phase 4)

One atomic Programmable Transaction Block, executed via `sui client ptb`:

```
0  coin_a::mint(CAP_A, input)                         -> in_a
1  executor::begin<A>(in_a, min_profit)               -> (coin, receipt)   [beg.0, beg.1]
2  amm_v2_adapter::swap_exact_in_a_to_b<A,B>(P_AB, beg.0, 0) -> cb
3  amm_v2_adapter::swap_exact_in_a_to_b<B,C>(P_BC, cb,   0) -> cc
4  amm_v2_adapter::swap_exact_in_a_to_b<C,A>(P_CA, cc,   0) -> ca
5  executor::settle<A>(beg.1, ca)                      // profit gate + payout
```

The hot-potato `ArbReceipt` (`beg.1`) forces command 5 to run, so the profit gate
cannot be skipped. Atomicity: a single transaction → any abort reverts all swaps.

## 4. Benchmark results (Phase 7)

**Off-chain, 100 consecutive scans** (release build, 3 pools, `max_hops=3`):

| Metric | avg | p50 | p99 | max |
|--------|-----|-----|-----|-----|
| Scan latency (`find_best`) | 17.95 µs | 16.38 µs | 42.50 µs | 42.50 µs |
| Simulation latency (route) | 0.55 µs | 0.54 µs | 1.88 µs | 1.88 µs |
| PTB build (plan assembly) | 3.39 µs | 2.54 µs | 49.63 µs | 49.63 µs |

**On-chain execution round-trip**, 5 real submissions (input 1e8, success):

| Run | Digest | Wall time |
|-----|--------|-----------|
| 1 | `F27Tz68eFGivDYbaucgQypdMw88cFFgVTMztAfyxBoZU` | 3.67 s |
| 2 | `D1de7v6P2rnSkNjrNGs4KojaVftBdQRwkax27ytBF3HM` | 3.58 s |
| 3 | `An6ymBujBAXSUczb1aoLGsCdMFgguroXQEBB3XpQvSjW` | 4.25 s |
| 4 | `CkoS98Nk7Yyx6a2TVvSiKaShnt6W4D9ZNTYmikEsuycb` | 4.05 s |
| 5 | `C56wUDaSDwgAGYDpbHX3MB4Yj4JgCkw8aWUYyBWJf6Ms` | 3.62 s |
| **avg** | | **3.83 s** |

> Caveat: the 3.83 s is **end-to-end via the `sui client` CLI** (cold process
> start + sign + submit + wait-for-finality). The bot's decision hot path
> (scan → simulate → build) totals **~22 µs**; a programmatic submitter using the
> Sui SDK over a warm connection removes the CLI startup and is bounded by network
> + consensus, not by our code.

## 5. Simulation accuracy (Phase 5)

Off-chain prediction from the production scanner (fed the on-chain reserves) vs the
real on-chain result. Arbitrage tx digest: `EatYbUoEZB1Cf3Eadfi1HnYirnMCuV49sPHWZpCf65jk`.

| Quantity | Simulated | Actual (on-chain) |
|----------|-----------|-------------------|
| Input (A) | 100,000,000,000 | 100,000,000,000 |
| Output (A) | 152,676,664,131 | **152,676,664,131** |
| Gross profit (A) | 52,676,664,131 | **52,676,664,131** |
| Gas (MIST) | 8,000,000 (est.) | 2,445,444 (actual) |

```
error_percentage = |52,676,664,131 − 52,676,664,131| / 52,676,664,131 = 0.000%   ✅ < 1%
```

Why exact: `sources/math.move` and `offchain/src/amm.rs` implement the identical
integer `x*y=k` formula. The off-chain simulator is a faithful mirror of the
on-chain VM for these swaps, so the predicted output equals the executed output
bit-for-bit. (Actual gas came in **well under** the 8 M estimate — the off-chain
`min_profit` margin is conservative, which is the safe direction.)

## 6. Failure test results (Phase 6)

| Case | Scenario | Expected | Observed | Digest |
|------|----------|----------|----------|--------|
| 1 | No arbitrage (A→B→A round trip) | abort | ✅ abort `executor::assert_profit` code 1 (E_INSUFFICIENT_PROFIT) | `2gxVX5SEPhYawevDpYXJsJY4gJJpJCbiKTPodFi2Hycr` |
| 2 | Profit < `min_profit` (target 60e9 > 52.6e9) | abort | ✅ abort `executor::assert_profit` code 1 | `AhHq8Mzdh5DFbeo8T9ws2yXcV755pAyeUPMTu837zpEq` |
| 3 | Gas estimate (60e9) > gross profit | reject off-chain | ✅ scanner returns `NO_OPP`, no tx | — (no submission) |
| 4 | Slippage: final-hop `min_out` impossibly high | revert | ✅ abort `amm_v2::swap_a_to_b` code 1 (E_SLIPPAGE) | `6jqMjCzsBBSuRrsGiEURQxJJaY1MLDgHvm5fcYahosAq` |

Case 2 verified on-chain: `Status: Failure { MoveAbort(... executor::assert_profit ..., 1) in command 5 }`.
Cases 1, 2, 4 are atomic reverts — only gas was charged, no funds moved.

## 7. Gas consumption analysis

| Operation | Gas (net MIST) | ≈ SUI |
|-----------|----------------|-------|
| Publish `validation_coins` | 32,291,480 | 0.0323 |
| Publish `arbitrage_system` | 37,299,880 | 0.0373 |
| Create 3 pools (1 PTB) | 7,618,384 | 0.0076 |
| **Arbitrage execution (full 3-hop)** | **2,445,444** | **0.00245** |

- A full triangular arbitrage costs **~0.0024 SUI** in gas — the metric that
  matters for live profitability. With `min_profit` set above this, every losing
  attempt aborts in `settle` and costs only gas.
- Total validation spend (2 publishes + pools + 1 main arb + 3 failure txs +
  5 timed execs): **~0.095 SUI** of the 1 SUI faucet grant (0.905 SUI remaining).
- Actual execution gas was **3.3× lower** than the conservative off-chain estimate
  (8 M), so trade sizing never under-charges itself.

## 8. Security observations

- **Profit gate held under adversarial inputs.** Cases 1 & 2 confirm `settle`
  aborts whenever `final < initial + min_profit`, using the *real* recorded input
  value — there is no path to keep coins and skip the check (hot-potato receipt).
- **Per-hop slippage independent of the profit gate.** Case 4 shows `min_out`
  aborts inside the adapter/AMM before settle, giving defense in depth.
- **Atomicity verified on-chain.** Failed PTBs reverted entirely; balance changes
  show no token movement on aborts, only gas.
- **Stateless executor confirmed.** The executor created no shared/owned objects;
  the only persistent objects on-chain are the pools (necessary) and capabilities.
- **Off-chain rejection works** (Case 3): an inflated gas estimate suppresses
  submission entirely, so unprofitable-after-gas routes never hit the chain.
- **Open items before mainnet:** dedicate a hot wallet with capped capital; pin
  `UpgradeCap` policy (`0x91c4…c565`); use a trusted fullnode for the reserve
  feed; remove/segregate `validation_coins` (testnet-only, mint-anything). See
  [SECURITY.md](SECURITY.md).

---

### Reproduction

```bash
# Phase 1
sui move build && sui move test
cd offchain && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check

# Phase 5 — off-chain prediction for the on-chain reserves
cargo run --example validate         # prints input / expected output / profit

# Phase 7 — offline latency benchmark
cargo run --release --example bench

# Phases 2–6 — publish, pools, PTBs: see commands in this report + deployment-testnet.md
```
