# sui-arbitrage-bot

Production-grade **atomic arbitrage & liquidity routing** on [Sui](https://sui.io),
tuned for micro-spreads and minimal gas.

- **On-chain (`arbitrage_system`, Move):** a *stateless* executor that enforces a
  single invariant — end with more base token than you started with — via a
  hot-potato profit gate. No AMM is hardcoded; venues plug in behind an adapter
  convention. Atomicity comes from running the whole route in one Programmable
  Transaction Block (PTB).
- **Off-chain (`offchain/`, Rust):** maintains a local pool cache, prices each hop
  with its native model (V2 `x*y=k` **or** CLMM tick-crossing engine), detects
  profitable cycles, re-prices the best candidate **authoritatively** on-chain, then
  dry-runs and (optionally) submits.

> ### Status (honest)
> - **On-chain:** executor + reference AMM + flash module complete; **real Cetus +
>   Turbos adapters** built against pinned **mainnet** interface packages. `sui move
>   test` **12/12**.
> - **Off-chain core:** CLMM-aware scanner (`PoolKind{V2,Clmm}`), authoritative-quote
>   seam, Scallop flash provider, frictions + risk models. `cargo test` **51/51**,
>   clippy `-D warnings` + fmt clean.
> - **Opportunity sources (one pipeline, no fork):** arbitrage, **liquidation**
>   (Scallop: index + oracle + health + sizing + liquidation PTB), and backrun-arb —
>   all emit the same `Opportunity` through the same dry-run → `RiskGuard` → submit
>   gate. See [docs/strategies-plan.md](docs/strategies-plan.md).
> - **Live path (`ws`/`quoter`/`executor`/`ptb::live`/`liquidation::{index,oracle}`):**
>   written against the real Sui SDK + venue/lender packages, **feature-gated
>   (`--features live`)**; compiles with the SDK but is *not* built/run in offline CI
>   here, and submit is **off by default** (`ARB_SUBMIT_ENABLED=false`).
> - **Execution verdict: NO-GO / measure-first.** Arb ≈ $17/day paper does not survive
>   frictions ([frictions-adjusted-pnl.md](docs/frictions-adjusted-pnl.md)). Liquidations
>   have a better (fat-tailed) payoff shape but need a live paper run to measure
>   frequency × capture before any submit
>   ([liquidation-pnl.md](docs/liquidation-pnl.md)). Capital is never at risk (dry-run +
>   on-chain `settle`/`repay` gates). Architecture:
>   [docs/consolidation-plan.md](docs/consolidation-plan.md).

## Layout

```
sui-arbitrage-bot/
├── Move.toml                       # package: arbitrage_system
├── sources/
│   ├── executor.move               # begin/settle — hot-potato profit gate (stateless)
│   ├── math.move                   # x*y=k swap math (shared, == offchain/amm.rs)
│   ├── amm_v2.move                 # reference UniV2-style pool (test target + UniV2 venue)
│   ├── admin.move                  # AdminCap capability
│   └── adapters/
│       ├── amm_v2_adapter.move     # adapter convention + in-package AMM
│       ├── cetus_adapter.move      # integration seam
│       └── turbos_adapter.move     # integration seam
├── tests/                          # 8 Move tests incl. full A→B→C→A cycle
├── offchain/                       # Rust scanner crate (arb-scanner)
│   ├── Cargo.toml                  # default = offline core; `live` = Sui SDK
│   └── src/{amm,types,cache,scanner,config}.rs   # quant core (tested)
│   └── src/{ws,ptb,executor}.rs                   # live integration (feature = "live")
└── docs/
    ├── ARCHITECTURE.md  GAS_OPTIMIZATION.md  SECURITY.md  TESTING.md
```

## Quick start

```bash
# On-chain (fetches pinned mainnet Cetus + Turbos interfaces on first build)
sui move build
sui move test                       # 12 passing

# Off-chain core (offline, no SDK)
cd offchain
cargo test                          # 36 passing
cargo run                           # demo scan: finds the seeded triangular arb
```

## Live mode (testnet)

```bash
sui client publish --gas-budget 100000000        # note the package id
cp .env.example .env                             # fill ARB_PACKAGE_ID etc.
cd offchain && cargo run --features live         # hydrate → stream → scan → submit
```

## Execution flow (one PTB, atomic)

```
split(input) → executor::begin → adapter::swap(A→B) → swap(B→C) → swap(C→A) → executor::settle
```

If the round trip doesn't clear `initial + min_profit`, `settle` aborts and the
entire PTB reverts — you pay only gas. Gas is estimated off-chain (dry-run) and
folded into `min_profit`; it is never modeled on-chain.

## Docs

- [Consolidation plan](docs/consolidation-plan.md) — the one canonical architecture (funnel: engine → authoritative quote → dry-run), data model, ingestion, venue scope
- [Strategies plan](docs/strategies-plan.md) — opportunity sources (arb / liquidation / backrun) on one pipeline; verified Scallop/Suilend liquidate APIs; indexing + oracle + health approach
- [Liquidation P&L](docs/liquidation-pnl.md) — liquidation methodology + race model + conditional go/no-go (measure-first)
- [Frictions-adjusted P&L](docs/frictions-adjusted-pnl.md) — latency + competition model on the paper baseline; the go/no-go (**NO-GO** for now)
- [Architecture](docs/ARCHITECTURE.md) — layers, PTB, adapters, flash-loan seam
- [Gas optimization checklist](docs/GAS_OPTIMIZATION.md)
- [Security review checklist](docs/SECURITY.md)
- [Testing strategy](docs/TESTING.md)
- [Testnet validation report](docs/testnet-validation-report.md) — sim == exec proof
- [External venue readiness audit](docs/external-venue-readiness-audit.md) — Cetus/Turbos/Kriya CLMM pricing, why `x*y=k` fails, the CLMM engine
- [Cetus parity validation](docs/cetus-parity-validation.md) — engine vs Cetus on-chain quoter: 1,034/1,034 exact across 27 mainnet pools
- [DeepBook + Aftermath study](docs/deepbook-aftermath-study.md) — next-gen market graph: native CLOB (parity PASS) + aggregator oracle, read-only
- [Momentum integration & study](docs/momentum-study.md) — Momentum CLMM read-only: discovery, authoritative quoter, parity PASS (<0.1%), integrated
- [Bluefin integration & study](docs/bluefin-study.md) — Bluefin Spot CLMM read-only: parity PASS (0.031%), integrated; most-active new venue (WAL surface)
- [Turbos parity research](docs/turbos-parity-research.md) — on-chain Turbos package/pool/quoter facts (friend fn → public `pool_fetcher`)
- [Turbos parity report](docs/turbos-parity-report.md) — engine vs authoritative: in-range PASS, multi-tick FAIL; does the 94%-Turbos profit survive?
- [Authoritative-pricing PnL report](docs/authoritative-pnl-report.md) — 5-venue dry-run, all CLMM legs authoritative: ~$17/day (active) vs ~$0 (overnight)

## Prerequisites

- Sui CLI: `brew install sui` (built against `framework/testnet`)
- Rust: stable toolchain (`cargo`, `rustfmt`, `clippy`)
