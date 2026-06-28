# Hand-off — 2026-06-28

Repository left in a clean, reproducible state. No live submission was performed; no funds
moved. This document is enough to resume tomorrow without reading chat history.

## Git
- **Branch:** `main`
- **Latest commit before this hand-off:** `2a7378e` (indexer debug log). Today's *code* work
  is already committed + pushed in three commits:
  - `3a719f5` feat(indexer): automated multi-DEX pool discovery (replaces ARB_TRACKED_POOLS)
  - `f48bc94` fix(indexer): discover via MoveEventModule, concurrent decode, default venue ids
  - `2a7378e` chore(indexer): log kept-pool liquidity distribution at debug level
- **This hand-off commit:** adds `HANDOFF.md` + `.gitignore` (ignore `.env.bak*`). (Hash filled in by the commit that introduces this file.)
- **Repository status:** working tree clean; no temporary/debug code left (all diagnostic
  logging was reverted; `git diff` is empty against the code commits).

## Files modified today (committed)
- `offchain/src/indexer.rs` (new) — automated CLMM pool indexer (Cetus + Turbos; DeepBook scaffold).
- `offchain/src/config.rs` — indexer config knobs; verified default Cetus/Turbos venue object ids.
- `offchain/src/main.rs` — run the indexer as a background task (replaces manual tracked-pool ingest).
- `offchain/src/metrics.rs` — `searcher_discovered_pools` + per-DEX gauges.
- `offchain/src/lib.rs` — register `indexer` module.
- `.env.example` — indexer documentation.
- `.gitignore`, `HANDOFF.md` (this commit).

## Configuration changed today (LOCAL only — `.env` is gitignored, NOT committed)
These live in `/Users/web3eg/Desktop/sui-arbitrage-bot/.env` (timestamped backups: `.env.bak*`):
- `ARB_FLASH_PROVIDER`: `mock` → `scallop`
- `ARB_FLASH_ENABLED`: `false` → `true`
- `ARB_SCALLOP_PACKAGE_ID`: (default `0xefe8b36d…`, stale) → **`0xde5c09ad171544aa3724dc67216668c80e754860f419136a68d78504eb2e2805`** (live upgraded Scallop package, confirmed in use 2026-06-27)
- `ARB_FLASH_LENDER_ID` = `0xa757975…` (Scallop Market), `ARB_FLASH_VERSION_ID` = `0x07871c4b…` (Scallop Version) — unchanged objects, duplicate keys cleaned
- `ARB_CETUS_GLOBAL_CONFIG_ID` = `0xdaa46292…`, `ARB_TURBOS_VERSIONED_ID` = `0xf1cf0e81…` (verified)
- `ARB_INDEXER_MIN_LIQUIDITY` = `100000000000000` (1e14)
- `ARB_MAX_DAILY_LOSS_USD` = `2`
- `ARB_PACKAGE_ID` unchanged (`0x443bc2…`); no Move change; no package republished.

## Problems solved today
1. **No pools (`ARB_TRACKED_POOLS=0`)** → built an automatic indexer; live run discovered
   **10,094** pools, kept **421** (top-liquidity Cetus+Turbos) in the scanner cache.
2. **`CommandArgumentError{arg_idx:0, TypeMismatch} in command 0`** → root-caused to a
   provider/lender mismatch (provider `mock` + a Scallop `Market` object, via duplicate
   `.env` keys). Fixed by switching to the Scallop provider + cleaning duplicates.
3. **`MoveAbort version::assert_current_version, 513, command 0`** → root-caused to a STALE
   Scallop package id (`0xefe8b36d…` is the original; protocol is now version 9). Fixed by
   pointing `ARB_SCALLOP_PACKAGE_ID` at the live upgraded package `0xde5c09ad…`.

## Current execution progress (one dry-run, submit OFF)
| Command | Step | Status |
|---|---|---|
| 0 | Scallop `flash_loan::borrow_flash_loan` (incl. `assert_current_version`) | ✅ pass |
| 1 | `executor::begin` | ✅ pass |
| 2 | first swap | ✅ pass |
| 3 | second swap (Cetus) | ❌ **MoveAbort `pool::swap_in_pool`, code 4** |

Last dry-run: `DryRun{ success:false, net_base:-1919668, gas_used:1919668 }` — reverts at
gas-only cost; **0 transactions submitted**.

## Remaining blocker
Command 3 aborts inside **Cetus** `pool::swap_in_pool` (runtime pkg `0x25ebb9a7…`, original
`0x1eabed72…`), **abort code 4**.

## Why command 3 currently fails (root cause, evidence-based)
The failing candidate is an **impossible over-quote**: input ≈ **120.4 SUI** → est. output ≈
**1,033.7 SUI** (≈ +913 SUI / 8.5×). This is a **Stage-1 CLMM quoting error**: discovered pool
state has **empty tick data** (`ticks: Vec::new()`), so the engine treats the whole pool as a
single infinite range at the current `sqrt_price` and massively over-quotes large inputs. The
real Cetus swap cannot deliver that output, so its internal swap math aborts (code 4) — *before*
our adapter's own `min_out` check. The dry-run gate therefore rejects it; **no capital is at risk**.
(Exact name of Cetus constant `4` not retrieved — confirm against Cetus `pool.move` for the
on-chain package version if needed.)

## Next recommended task (ONE only)
**Implement CLMM tick ingestion for Cetus** so Stage-1 quotes traverse real initialized ticks
instead of an empty single range. Start with **Cetus only**, and validate against the single
failing candidate — do not build a full multi-venue system first; the goal is to prove the
empty-ticks hypothesis and make the impossible `120→1033 SUI` quote disappear.

### Estimated scope
**Medium, ~1 focused session.** Read the Cetus pool object's tick manager / tick table /
bitmap (dynamic fields), fetch ticks around the current price, feed `liquidity_net` per tick
into the `clmm` engine's exact-in traversal, add diagnostics (current tick, sqrt price, ticks
loaded, amount remaining per crossing, quote before/after), then re-run the same candidate in
dry-run. Turbos is left unchanged until Cetus is proven (it may use a different tick layout).

## Risks before starting tomorrow
1. **`ARB_SUBMIT_ENABLED=true` is set in `.env`.** Today it never submitted only because every
   dry-run reverts at command 3. Once command 3 is fixed, a passing dry-run would **auto-submit
   real funds**. **Recommend setting `ARB_SUBMIT_ENABLED=false`** before any tick-ingestion
   testing (or always pass it as an env override). Sender `0x1fb09e…` holds ~10 SUI.
2. **`.env` is gitignored** — the live config above is local only. If `.env` is lost, restore
   from `.env.bak*` and re-apply the values listed above.
3. **Scallop package freshness** — `0xde5c09ad…` was confirmed live 2026-06-27. If
   `assert_current_version` (513) ever returns, re-verify the current package id on chain.
4. **Indexer `MIN_LIQUIDITY=1e14`** makes real candidates rare; for tick-ingestion testing,
   override `ARB_INDEXER_MIN_LIQUIDITY=0` to reproduce the failing candidate quickly.
5. **Profitability still unproven** — reaching command 3 does not imply an edge exists; the
   over-quotes are artifacts. Real-edge validation is a separate, later question.
