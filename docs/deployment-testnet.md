# Testnet Deployment Artifacts

Network: **Sui Testnet** — `https://fullnode.testnet.sui.io:443`
Date: 2026-06-21 · Toolchain: `sui 1.73.1`, `rustc/cargo 1.96.0`

## Accounts & funding

| Item | Value |
|------|-------|
| Deployer address | `0x1fb09ebea3122ebf3abcf063b961c773c4ae78dd39413c265358051af347dd08` |
| Faucet tx (1 SUI) | `BUzTWsUs2zMkheqKVgn2raFdQSFxtBoKVDQ6iTf9de1A` |

## Package: `arbitrage_system` (audited core)

| Item | Value |
|------|-------|
| Package ID | `0x5de0227d4455506b87ad26af9cb615a6d51f50668091f3066172782bab4521ea` |
| Publish digest | `DbuJvKXJUdCxgZYCbRcd5ekxqxqZQts29sN8iMcPTt1Q` |
| AdminCap | `0x01184ef485dca97d9f047d1e539cefc94f09096b93742821dde38008ae36a896` |
| UpgradeCap | `0x91c4555677cccdfc117dd0e67ec8991b2b507d33f6ef2469f17e3dd63457c565` |
| Publish gas (MIST) | 37,299,880 |

Published module IDs (`<package>::<module>`):
- `…4521ea::executor`
- `…4521ea::math`
- `…4521ea::amm_v2`
- `…4521ea::admin`
- `…4521ea::amm_v2_adapter`
- `…4521ea::cetus_adapter`
- `…4521ea::turbos_adapter`

## Package: `validation_coins` (testnet-only helper)

Three mintable coins used to seed pools. Not part of the audited package.

| Item | Value |
|------|-------|
| Package ID | `0x60e43310fb4817f447afd819ba95beb6afea3f90489ed42f0f284b1cd545d6bc` |
| Publish digest | `GJZCeoyaTeVHVvpBJRJ8ot39M98nxRyMkNQNEoQPB88N` |
| TreasuryCap&lt;COIN_A&gt; | `0xf11f01a8c7662fac8666388676997b60e8b0d38e4318f48bb96eac55af1a808c` |
| TreasuryCap&lt;COIN_B&gt; | `0xbf7bf80c83dddd793e415fb0f21a3377f65777af6b3543be40c96d959305c964` |
| TreasuryCap&lt;COIN_C&gt; | `0xf3166b5dcd88b081f9458ff3a32fc5153e18ae1aa8c4a01b5bfac21b06a40baf` |
| Publish gas (MIST) | 32,291,480 |

Coin type tags:
- A = `0x60e4…d6bc::coin_a::COIN_A`
- B = `0x60e4…d6bc::coin_b::COIN_B`
- C = `0x60e4…d6bc::coin_c::COIN_C`

## Pools (shared objects)

Created in one PTB · digest `EtqSXftCWMC69aNQoQgFsQXZzCgvAp2A7wXgTot9L5Rc` · gas 7,618,384 MIST.

| Pool | Object ID | reserve_a | reserve_b | fee |
|------|-----------|-----------|-----------|-----|
| A/B (`Pool<A,B>`) | `0xacb6790b25f3281c8ed4bd81c741ef9a26ce379c82814293fd7cb166e499943a` | 1,000,000,000,000 A | 1,000,000,000,000 B | 30 bps |
| B/C (`Pool<B,C>`) | `0xad2bed9591cc6ef875a5ca3ffee3bbf954baf347f160d955f69ac7e1c44e4174` | 1,000,000,000,000 B | 1,000,000,000,000 C | 30 bps |
| C/A (`Pool<C,A>`) | `0x8a4fbc5255a2607a97b40c40f4a434af7ce0b74df1c6bfb8043722edc947be77` | 1,000,000,000,000 C | 2,000,000,000,000 A | 30 bps |

Implied prices: 1 A = 1 B, 1 B = 1 C, **1 C = 2 A** → the A→B→C→A cycle is
intentionally profitable (~2× gross before fees and price impact).
