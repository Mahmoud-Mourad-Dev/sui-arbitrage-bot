# sui-arbitrage-bot

Production-grade **atomic arbitrage & liquidity routing** on [Sui](https://sui.io),
tuned for micro-spreads and minimal gas.

- **On-chain (`arbitrage_system`, Move):** a *stateless* executor that enforces a
  single invariant — end with more base token than you started with — via a
  hot-potato profit gate. No AMM is hardcoded; venues plug in behind an adapter
  convention. Atomicity comes from running the whole route in one Programmable
  Transaction Block (PTB).
- **Off-chain (`offchain/`, Rust):** subscribes to pool updates, maintains a local
  reserve cache, simulates `x*y=k` swaps, detects profitable cycles, builds &
  dry-runs PTBs, and submits **only** profitable transactions.

> Status: Move package + reference AMM + executor are complete and tested (8 Move
> tests). Rust quant core (math, cache, scanner) is complete and tested (7 tests,
> clippy/fmt clean). The live Sui-SDK integration (`ws`/`ptb`/`executor`) is a
> faithful, feature-gated seam — see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

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
# On-chain
sui move build
sui move test                       # 8 passing

# Off-chain quant core (offline, no SDK)
cd offchain
cargo test                          # 7 passing
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

## Prerequisites

- Sui CLI: `brew install sui` (built against `framework/testnet`)
- Rust: stable toolchain (`cargo`, `rustfmt`, `clippy`)
