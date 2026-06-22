# Turbos Parity Research (Phase 1)

Everything below is grounded from **live mainnet on-chain data** (RPC
`getObject` / `getNormalizedMove*` / a real swap tx), not the SDK.

## Mainnet package IDs

| Item | Value |
|------|-------|
| Type-origin package | `0x91bfbc386a41afcfd9b2533058d7e915a1d3829089cc268ff4333d54d6339ca1` |
| **Latest package (for calls)** | `0xa5a0c25c79e428eba04fb98b3fb2a34db45ab26d4c8faf0d7e39d66a63891e64` |
| `Versioned` shared object | `0xf1cf0e81048df168ebeb1b8030fad24b3e0b53ae827c25053fff0779c1445b6f` (isv 1621135) |
| Clock | `0x6` |

**Critical:** Turbos has been upgraded. Calling the quoter on the *origin* package
aborts in `pool::check_version` (sub-status 23). The **latest** package id must be
used for move calls (found from a recent `swap_router` MoveCall). Pool objects and
coin types still reference the origin package.

## Pool structure

`pool::Pool<CoinTypeA, CoinTypeB, FeeType>` — **3 type parameters** (the fee tier is
a phantom type, e.g. `fee500bps::FEE500BPS`). Relevant fields:

| Field | Type | Meaning |
|-------|------|---------|
| `coin_a`, `coin_b` | Balance | pool reserves (book balances, not the curve) |
| `sqrt_price` | u128 | **Q64.64** sqrt price (X64), same family as Cetus/Bluefin |
| `liquidity` | u128 | active liquidity `L` (UniV3) |
| `tick_current_index` | `i32::I32 { bits: u32 }` | current tick (signed via two's-complement bits) |
| `tick_spacing` | u32 | tick spacing |
| `fee` | u32 | swap fee in **pips (/1e6)** — see below |
| `fee_protocol` | u32 | protocol's *share of the fee* (300000 = 30%), NOT additional |
| `tick_map` | Table | tick storage (no public reader — see Ticks) |
| `unlocked` | bool | pool active flag |

## Tick structure
Ticks live in `tick_map` (a `Table`/bitmap). **There is no public `fetch_ticks`** on
Turbos (unlike Cetus). Consequence: an off-chain engine cannot load Turbos tick
liquidity to price cross-tick swaps — it can only price the **current active range**.

## Liquidity representation
Standard UniV3 active liquidity `L` (u128). Within one tick range the pool behaves
as `x·y=k` on virtual reserves `x=L·2⁶⁴/√P`, `y=L·√P/2⁶⁴`.

## Fee representation
`fee` is in **pips** (denominator 1e6). The tier names are misleading: `FEE500BPS`
has `fee=500` = **0.05%** (not 500 bps). Verified empirically: SUI/USDC@`fee=500`
quote loses ~0.05% vs mid. `fee_protocol=300000` is 30% of the swap fee routed to the
protocol — it does **not** change the taker's effective fee. The engine's
`fee_pips = pool.fee` reproduces Turbos exactly in-range (verified to 1.4e-7 on
WAL/SUI@2500).

## Sqrt price format
Q64.64 fixed point (X64). Engine bounds: MIN `4295048016`,
MAX `79226673515401279992447579055`.

## Authoritative quote function(s)

- `pool::compute_swap_result` — **`friend` visibility → NOT callable** from a PTB /
  dev-inspect. (This is why Turbos was never validated before.)
- **`pool_fetcher::compute_swap_result`** — `public entry`, the usable authoritative
  quoter:

```
pool_fetcher::compute_swap_result<CoinA, CoinB, FeeType>(
    pool: &mut Pool, a_to_b: bool, amount: u128, by_amount_in: bool,
    sqrt_price_limit: u128, clock: &Clock, versioned: &Versioned, ctx: &mut TxContext,
) -> ComputeSwapState
```

`ComputeSwapState` (all u128): `amount_a, amount_b, amount_specified_remaining,
amount_calculated, sqrt_price, tick_current_index, fee_growth_global, protocol_fee,
liquidity, fee_amount`. The output is **`amount_calculated`** (BCS offset 48). Set
`by_amount_in=true` for exact-in, `false` for exact-out. Read-only via dev-inspect
(no signing, no state commit). Implemented in
[`validation/cetus/turbos_rpc.py`](../validation/cetus/turbos_rpc.py).
