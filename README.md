# sui-arbitrage-bot

A minimal [Sui](https://sui.io) Move package scaffold — ready to build on.

## Structure

```
sui-arbitrage-bot/
├── Move.toml              # Package manifest (name, edition, deps, addresses)
├── sources/
│   └── scaffold.move      # Example module — replace with your own
└── tests/
    └── scaffold_tests.move # Example test
```

## Prerequisites

Install the Sui CLI (not yet installed on this machine):

```bash
# macOS (Homebrew)
brew install sui

# or via cargo
cargo install --locked --git https://github.com/MystenLabs/sui.git --branch testnet sui
```

Verify:

```bash
sui --version
```

## Build & test

```bash
cd ~/Desktop/sui-arbitrage-bot

sui move build        # compile
sui move test         # run tests
```

## Publish (testnet)

```bash
# Configure a testnet env + address first:
#   sui client new-env --alias testnet --rpc https://fullnode.testnet.sui.io:443
#   sui client switch --env testnet
#   sui client faucet                # get test SUI

sui client publish --gas-budget 100000000
```

## Next steps

- Rename the package in `Move.toml` (`name` and the named address under `[addresses]`).
- Replace `Greeting` in `sources/scaffold.move` with your own objects and logic.
- Pin a specific framework `rev` in `Move.toml` for reproducible builds.
