# Live validation runbook (Phases 5–6) — operator-executed

The code is built + unit-verified (build/clippy/test green on both feature sets). The
remaining validation **requires resources only the operator has**: a published package,
a funded keystore, and RPC. This runbook is the exact sequence to run it. **Nothing here
is claimed as done** — it is the procedure for *you* to execute and record.

Guard rails baked in: `ARB_SUBMIT_ENABLED=false` by default (build + dry-run, never
sign), the on-chain `settle`/`repay` gates, and the `RiskGuard` (kill switch +
daily-loss cap + pool blacklist). Never set `ARB_SUBMIT_ENABLED=true` on mainnet without
the Phase-6 numbers (see [frictions-adjusted-pnl.md](frictions-adjusted-pnl.md) /
[liquidation-pnl.md](liquidation-pnl.md)).

---

## Two paths (pick per goal)

| | **A. Mainnet dry-run-paper (recommended)** | **B. Testnet real adapter swap** |
|---|---|---|
| Goal | Phase 6: exercise ingest → scan → authoritative re-quote → build → **dry-run** → `record_realized`, on real pools, **no trade** | Phase 5: land **one real Cetus adapter swap** + one full submitted cycle |
| Move deps | as-is (`framework/mainnet`, Cetus `mainnet-v1.52.3`, Turbos pinned) | must **re-pin to testnet** Cetus/Turbos + framework/testnet |
| Cost/risk | publish gas only; **submit stays off** → no trade risk | publish gas + a real (tiny) swap; needs funded testnet key |
| Why recommended | the package is already mainnet-configured; dry-run needs no funded trading and gives the Phase-6 dataset | proves the adapter end-to-end, but more setup + real funds |

Do **A** first (it's the measurement the go/no-go needs). Do **B** only when you want a
landed-tx proof of a real adapter leg.

---

## Prerequisites (both paths)
- `sui` CLI ≥ 1.73, a keystore with a funded address (`sui client active-address`,
  `sui client gas`).
- The signing key stays in the Sui file keystore (`~/.sui/sui_config/sui.keystore`);
  it is read via `FileBasedKeystore` and **never logged / never committed**.

## Path A — mainnet dry-run-paper (Phase 6)

```bash
# 1. Publish the package (records the package id; costs gas, no trade).
cd ~/Desktop/sui-arbitrage-bot
./scripts/publish.sh            # prints ARB_PACKAGE_ID; uses your active env (set to mainnet)

# 2. Configure (.env). Fill from the publish output + the object ids below.
cp .env.example .env
#   SUI_RPC_URL=https://fullnode.mainnet.sui.io:443
#   ARB_PACKAGE_ID=<from publish>
#   ARB_SENDER_ADDRESS=<your address>     ARB_KEYSTORE_PATH=~/.sui/sui_config/sui.keystore
#   ARB_TRACKED_POOLS=cetus:0x..,turbos:0x..     (the pools from validation/cetus/mv_pools.json)
#   ARB_CETUS_GLOBAL_CONFIG_ID=0x...   ARB_TURBOS_VERSIONED_ID=0x...
#   ARB_SUBMIT_ENABLED=false              # <-- keep false for Phase 6
#   (flash/liquidation ids only if exercising those)

# 3. Run the live pipeline (dry-run only). It hydrates pools, scans, authoritatively
#    re-quotes, builds the PTB, dry-runs it, and logs the gated decision — never signs.
cd offchain && cargo run --features live 2>&1 | tee ../validation/cetus/mainnet_dryrun.log

# 4. Let it run a meaningful window; collect the dry-run net distribution + (for
#    liquidations) frequency × simulated capture. Feed into the frictions model and
#    update docs/frictions-adjusted-pnl.md + docs/liquidation-pnl.md with REAL numbers
#    and an explicit go/no-go. (`realized_vs_predicted` here is modeled, since nothing
#    submits — document it as such.)
```

Acceptance (A): a mainnet dry-run dataset (`mainnet_dryrun.log`) + refreshed PnL docs
with an honest verdict. Capital never at risk.

## Path B — testnet real adapter swap (Phase 5)

```bash
# 1. Re-pin Move deps to testnet (Move.toml): framework/testnet, and the testnet
#    Cetus/Turbos interface revs + testnet `cetusclmm`/`turbos_clmm` addresses.
#    Re-pin the offchain SDK similarly if you want testnet RPC types (the client API is
#    network-agnostic, so RPC URL alone usually suffices).
# 2. Publish to testnet:  ./scripts/publish.sh   (active env = testnet; faucet-funded key)
# 3. Configure .env with testnet RPC + testnet pool/config object ids + your address.
# 4. One real Cetus adapter swap leg (proves the adapter end-to-end):
#    build a PTB that calls cetus_adapter::swap_exact_in_a_to_b against a real testnet
#    Cetus pool and execute it; record the digest + effects.
# 5. One full submitted cycle: set ARB_SUBMIT_ENABLED=true on a seeded/known opportunity,
#    let try_execute go build → dry-run → submit → effects → record_realized; record the
#    digest. Then set it back to false.
```

Acceptance (B): a testnet digest for a real Cetus adapter swap **and** for one full
submitted cycle; update [testnet-validation-report.md](testnet-validation-report.md) to
add a *live-path* section reflecting exactly what was exercised (keep the reference-AMM
section labeled as such).

---

## Object ids you'll need (resolve fresh; do not hardcode versions)
- **Pools:** from `validation/cetus/mv_pools.json` (already discovered) or `mv_scan.py discover`.
- **Cetus `GlobalConfig`**, **Turbos `Versioned`**, **Clock** `0x6`: shared objects; the
  bot resolves each object's `initial_shared_version` at build time (`resolve_shared`),
  so you only supply the **ids**.
- **Scallop (only if exercising flash/liquidation):** Market (`ARB_FLASH_LENDER_ID`),
  Version (`ARB_FLASH_VERSION_ID`), x_oracle, registry, package — verified ids in
  `flashloan::scallop_pins` + `config`.

## What the bot will NOT do
- Sign or submit anything while `ARB_SUBMIT_ENABLED=false` (default).
- Route through a blacklisted pool, exceed the daily-loss cap, or run with the kill
  switch on.
- Liquidation submit: `executor` currently routes liquidation to this runbook (full
  oracle-object resolution + the Pyth accumulator→HotPotatoVector constructor are the
  on-chain items to confirm here before enabling).
