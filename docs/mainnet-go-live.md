# Mainnet go-live — staged runbook (operator-executed)

> **Read this first.**
> 1. **The measured edge is marginal (NO-GO).** Arb nets ~$1–3/day after frictions; the
>    long tail is gas-negative ([frictions-adjusted-pnl.md](frictions-adjusted-pnl.md)).
>    Do not expect profit. Going live starts in **dry-run** to *measure*, not to earn.
> 2. **The live path has never run on-chain.** It compiles and is unit-tested; you are
>    the first real execution. Budget for debugging + gas.
> 3. **Arb only, submit OFF, first.** Liquidations are **not** mainnet-ready (the Pyth
>    price-update step is unfinished — a liquidation submit will abort). Keep
>    `ARB_LIQ_ENABLED=false`.
>
> Capital downside is capped at **gas** by the on-chain `settle` gate (a bad fill reverts),
> but gas bleed + MEV competition are real. Enable real submit only after Step 5's data.

## Step 0 — Prerequisites
```bash
sui --version                      # >= 1.73
sui client switch --env mainnet    # or: sui client new-env --alias mainnet --rpc https://fullnode.mainnet.sui.io:443
sui client active-env              # must say mainnet
sui client active-address          # your deployer/signer
sui client gas                     # need SUI: ~0.2 for publish + gas per tx
```
The signing key stays in `~/.sui/sui_config/sui.keystore` — read locally, never logged.

## Step 1 — Build & test locally (free)
```bash
cd ~/Desktop/sui-arbitrage-bot
sui move build && sui move test            # 12/12
cd offchain && cargo build --features live # first build compiles the Sui SDK (slow)
```

## Step 2 — Publish to mainnet (~0.2 SUI gas, no trade)
```bash
cd ~/Desktop/sui-arbitrage-bot
./scripts/publish.sh            # confirms env, publishes, prints ARB_PACKAGE_ID
```
Save the printed `ARB_PACKAGE_ID`.

## Step 3 — Configure `.env`
```bash
cp .env.example .env
```
```ini
SUI_RPC_URL=https://fullnode.mainnet.sui.io:443
SUI_WS_URL=wss://fullnode.mainnet.sui.io:443
ARB_PACKAGE_ID=<from step 2>
ARB_BASE_TOKEN=0x2::sui::SUI
ARB_EXECUTION_MODE=owned        # simplest first live path; no lender dependency
ARB_FLASH_ENABLED=false
ARB_SENDER_ADDRESS=<your active-address>
ARB_KEYSTORE_PATH=/Users/<you>/.sui/sui_config/sui.keystore
ARB_TRACKED_POOLS=cetus:0x..,turbos:0x..     # ids from validation/cetus/mv_pools.json
ARB_CETUS_GLOBAL_CONFIG_ID=0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f
ARB_TURBOS_VERSIONED_ID=0xf1cf0e81048df168ebeb1b8030fad24b3e0b53ae827c25053fff0779c1445b6f
ARB_MIN_PROFIT=50000000          # 0.05 SUI floor — filters the gas-negative thin tail
ARB_GAS_BUDGET=30000000
ARB_SUBMIT_ENABLED=false         # KEEP FALSE for Steps 4–5
ARB_KILL_SWITCH=false
ARB_MAX_DAILY_LOSS_USD=5
ARB_SUI_PRICE_USD=<current operator price; required for USD risk accounting>
ARB_LIQ_ENABLED=false            # liquidations not mainnet-ready yet
```
> **Both venue ids above were verified against mainnet** (read-only `sui_getObject`):
> `0xdaa462…a3d8f` → `0x1eabed72…::config::GlobalConfig` (shared), and
> `0xf1cf0e81…45b6f` → `0x91bfbc38…::pool::Versioned` (shared) — matching the exact
> Cetus/Turbos packages the adapters call. Clock `0x6` and every object's version are
> resolved automatically (you supply only ids). Pool ids:
> `cd validation/cetus && python3 mv_scan.py discover`.

## Step 4 — Dry-run-paper (submit OFF — zero capital risk)
```bash
cd offchain
cargo run --features live 2>&1 | tee ../validation/cetus/mainnet_dryrun.log
```
Per tick: hydrate pools → scan → **authoritative re-quote** → build the real PTB →
**`dry_run_transaction_block`** → log the gated decision. With `submit_enabled=false` it
prints `DRY-RUN ONLY … stopping before signing` and **never signs**. Run across active
market hours. Watch: pools hydrate, dry-runs succeed, net-after-gas values.

## Step 5 — Measure & decide (the real go/no-go)
From `mainnet_dryrun.log`: the distribution of dry-run net (after real gas) and how often
it clears `ARB_MIN_PROFIT`. If profitable hits are rare/small (the expected outcome),
**stop — you have your answer, at zero risk.**

## Step 6 — ONLY if the data justifies it: enable submit at minimal size
```ini
ARB_SUBMIT_ENABLED=true
ARB_MIN_PROFIT=<high enough to clear gas + competition with margin>
ARB_CANDIDATE_INPUTS=100000000,500000000   # small first
ARB_MAX_DAILY_LOSS_USD=5
```
Run `cargo run --features live`. Each landed tx still passes the on-chain `settle` gate
(bad fill → revert → gas only). **Monitor `realized_vs_predicted`** — well under 1 means
competition is eating you; stop. `ARB_KILL_SWITCH=true` halts instantly.

## Gotchas
- **Liquidations off** on mainnet (Pyth accumulator step unverified → abort).
- **Gas coins:** keep a few separate SUI coins so gas doesn't collide with trade inputs.
- **First submit:** flip it on one obvious opportunity at tiny size to confirm the
  adapter lands, then let it run.
- **Don't run submit overnight** — edge ≈ $0 overnight; it just bleeds gas.
