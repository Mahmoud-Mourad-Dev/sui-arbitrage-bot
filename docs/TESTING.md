# Testing Strategy

Two test pyramids that meet in the middle: Move proves the on-chain invariant;
Rust proves detection/sizing; both share the **same x*y=k math and the same
numeric fixture** (the "C ≈ 2A" triangle) so a green pair means simulation
matches execution.

## On-chain (Move) — `sui move test`

Current suite (8 tests, all passing):

| Test | Proves |
|------|--------|
| `math` parity | `get_amount_out` equals UniswapV2 (500 @ 0 fee, 499 @ 30 bps) |
| `executor::settle_pays_out…` | profitable route returns principal + profit |
| `executor::settle_passes_at_exact_target` | boundary: `final == initial + min_profit` allowed |
| `executor::settle_aborts_when_below_target` | abort code 1 when under target |
| `amm_v2::swap_moves_reserves…` | swap output + reserve accounting correct |
| `amm_v2::swap_aborts_on_slippage` | `min_out` floor enforced |
| `arb_route::atomic_three_hop_cycle_is_profitable` | full A→B→C→A via adapter + executor yields profit |
| `arb_route::unprofitable_cycle_aborts_atomically` | unrealistic `min_profit` → whole PTB reverts |

To add as venues are wired:
- Negative tests per adapter (`E_ADAPTER_NOT_CONFIGURED` until pinned).
- Fuzz `get_amount_out` against a u128 reference for overflow/rounding.
- Multi-pool contention scenarios in `test_scenario`.

## Off-chain (Rust) — `cargo test`

Current suite (7 tests, all passing):

| Test | Proves |
|------|--------|
| `amm::parity_with_move_math` | Rust math == Move math (same 500 / 499) |
| `amm::degenerate_inputs_return_none` | no panics on bad input |
| `amm::amount_in_round_trips` | `get_amount_in` never under-quotes |
| `cache::upsert_get_update` | reserve cache CRUD + unknown-pool guard |
| `scanner::finds_profitable_triangle` | detects the 3-hop opportunity |
| `scanner::no_opportunity_when_balanced` | no false positives on 1:1 pools |
| `scanner::gas_cost_can_erase_thin_profit` | gas accounting kills marginal trades |

## Integration (live, manual)

1. `sui move test && (cd offchain && cargo test)` — must be green.
2. Publish to **testnet**; create 3 `amm_v2` pools with a known dislocation.
3. `ARB_PACKAGE_ID=… cargo run --features live` — confirm the scanner finds the
   route, dry-run shows profit, and a submitted PTB lands with an `ArbExecuted`
   event.
4. Negative: balance the pools and confirm the bot submits nothing (or aborts in
   `settle` if it races a price move) — losing only gas.

## CI gate (recommended)

```
sui move build && sui move test
cd offchain && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
```
