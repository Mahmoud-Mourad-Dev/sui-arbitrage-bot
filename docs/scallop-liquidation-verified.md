# Scallop liquidation — verified on-chain ground truth

**Method.** Everything in the **VERIFIED** sections below was read directly from Sui
mainnet (public fullnode `https://fullnode.mainnet.sui.io:443`) on 2026-06-28 via
`sui_getNormalizedMove*`, `sui_getObject`, `suix_getDynamicFields[Object]`, and
`sui_getTransactionBlock`. Nothing here is inferred.

**Primary evidence**
- Successful liquidation tx: `AYdhgWMq73LBs7pnbJLsHxtDWXneERfbWHpznwGCBKN5`
  (4 more for cross-check: `GLh3JmDVJ4ih7rzXhQiopmqLZFvGJEsHSNjEbdzFa5X3`,
  `6oPtXS9tLrR5qGVMTUPXz5VK4yG562Z667FUN5qPemsY`,
  `Gbc9xrSs3rQVnD4jdYsnCgq2Sb5xGtjp7xmKx4244uLf`,
  `3EXj5uS2H465dALYo3wqrmwkX1K9Ao4n89XKQ7Agh6MG`).
- Live obligation object: `0xa042dcfdb81ffc562537baee6b9820fb515ce0a207a71b9b121639fcb9661577`.

Each item is tagged **VERIFIED** (read from chain) or **ASSUMPTION / REMAINING** (still
needs a live submit or dry-run to confirm).

---

## 1. VERIFIED — `liquidate` signature & returns
Package `0xefe8b36d…` (Scallop `protocol`), module `liquidate`:

```move
public fun liquidate<DebtType, CollateralType>(
    version:  &Version,                 // 0xefe8b36d…::version::Version
    obligation: &mut Obligation,        // 0xefe8b36d…::obligation::Obligation
    market:   &mut Market,              // 0xefe8b36d…::market::Market
    repay:    Coin<DebtType>,           // the debt coin to repay
    registry: &CoinDecimalsRegistry,    // 0xca5a5a62…::coin_decimals_registry::CoinDecimalsRegistry
    x_oracle: &XOracle,                 // 0x1478a432…::x_oracle::XOracle
    clock:    &Clock,                   // 0x2::clock::Clock
    ctx:      &mut TxContext,
): (Coin<DebtType>, Coin<CollateralType>)   // (leftover repay, seized collateral)
```
- **2 type params**: `DebtType`, `CollateralType`.
- `liquidate` is `public` (composable in a PTB) and **returns** `(Coin<Debt>, Coin<Collateral>)`.
  `liquidate_entry` is the `entry` wrapper (not used by searchers).

## 2. VERIFIED — obligation layout & decode  ✅ implemented
`obligation::Obligation` fields: `id, balances, debts, collaterals, rewards_point,
lock_key, borrow_locked, repay_locked, deposit_collateral_locked,
withdraw_collateral_locked, liquidate_locked`.

`collaterals` / `debts` are `wit_table::WitTable<…, 0x1::type_name::TypeName, V>`:
```
WitTable { id: UID,  table: 0x2::table::Table<TypeName, V>,  keys: Option<VecSet<TypeName>>,  with_keys: bool }
```
- **The entries hang off the inner `table`'s UID**, NOT the WitTable's own `id`
  (the WitTable UID has **0** dynamic fields — confirmed). Path:
  `obligation.content.fields.{collaterals,debts}.fields.table.fields.id.id`.
- Each entry is a dynamic field: name = `TypeName` (coin type, **no `0x` prefix** in the
  `value.name`), value = `obligation_collaterals::Collateral { amount: u64 }` or
  `obligation_debts::Debt { amount: u64, borrow_index: u64 }`, at `value.fields.amount`.

This is implemented in [`liquidation/index.rs`](../offchain/src/liquidation/index.rs)
(`extract_table_id` → descends into `table`; `read_positions` enumerates dynamic fields;
`amount_from_value`/`coin_type_from_name` pure-tested) and proven end-to-end by the
ignored test `decode_real_mainnet_obligation` (decodes the live obligation above:
collateral HASUI/USDT/COIN, debt USDC/DEEP/SUI/WAL/SCA/FDUSD/ETH, real amounts).

## 3. VERIFIED — full liquidation PTB (tx `AYdhgWMq…`, 19 commands)
`Ix` = input, `Rn`/`NR[n,k]` = (nested) result of command n. Clock = `I2` = `0x6`.

```
# Pyth price update (Wormhole VAA → accumulator → update feed)
[0] wormhole::vaa::parse_and_verify(wormhole_state=I0, vaa_bytes=I1, clock=I2)               -> R0
[1] pyth::create_authenticated_price_infos_using_accumulator(
        pyth_state=I3, accumulator_msg=I4, verified_vaas=NR[0,0], clock=I2)                  -> R1: HotPotatoVector<PriceInfo>
[2] SplitCoins(Gas, [I5=1])                                                                  -> R2: fee coin (1 MIST)
[3] pyth::update_single_price_feed(
        pyth_state=I3, hpv=NR[1,0], price_info_object=I6, fee=NR[2,0], clock=I2)             -> R3: HotPotatoVector<PriceInfo>
[4] pyth::hot_potato_vector::destroy<PriceInfo>(NR[3,0])

# Flash-borrow the debt asset, liquidate, swap seized collateral back, repay
[5] scallop::flash_loan::borrow_flash_loan<Debt>(version=I7, market=I8, amount=I9)           -> R5: (Coin<Debt>=NR[5,0], Receipt=NR[5,1])
[6] scallop::liquidate::liquidate<Debt,Coll>(
        version=I7, obligation=I10, market=I8, repay=NR[5,0],
        registry=I11, x_oracle=I12, clock=I2)                                                -> R6: (Coin<Debt> leftover=NR[6,0], Coin<Coll> seized=NR[6,1])
[7] TransferObjects([NR[6,0]], recipient=I13)            # return leftover repay to sender
[8] cetus::pool::flash_swap<Coll,Debt>(global_config=I14, pool=I15, a2b=I16, by_amount_in=I17,
        amount=I18, sqrt_price_limit=I19, clock=I2)                                          -> R8: (Coin/Bal..., Receipt=NR[8,2])
[9..16] split/into_balance/zero/repay_flash_swap/from_balance/transfer  # settle Cetus flash swap of seized collateral → debt
[17] scallop::flash_loan::repay_flash_loan<Debt>(version=I7, market=I8, coin=NR[16,0], receipt=NR[5,1])
[18] TransferObjects(...)                                # final profit to sender
```
Key orderings to match exactly: `liquidate` arg order is **(version, obligation, market,
repay_coin, registry, x_oracle, clock)**; `borrow_flash_loan`/`repay_flash_loan` are
`(version, market, amount)` / `(version, market, coin, receipt)`.

## 4. VERIFIED — Pyth accumulator → HotPotatoVector flow
The "accumulator → HotPotatoVector" construction the build needs is **exactly**:
1. `wormhole::vaa::parse_and_verify(wormhole_state, vaa_bytes, clock) -> VerifiedVAA[]`
   — `vaa_bytes` come from Hermes (`fetch_pyth_vaa`, already in `oracle.rs`).
2. `pyth::create_authenticated_price_infos_using_accumulator(pyth_state, accumulator_msg,
   verified_vaas, clock) -> HotPotatoVector<PriceInfo>` — `accumulator_msg` is the second
   Hermes blob.
3. For **each** price feed: `pyth::update_single_price_feed(pyth_state, hpv,
   price_info_object_for_feed, fee_coin, clock) -> HotPotatoVector` (threads the hot potato).
4. `pyth::hot_potato_vector::destroy<PriceInfo>(hpv)` to consume it.
Then `liquidate` reads `&XOracle`, which consults the now-fresh Pyth `PriceInfoObject`s.
**No `x_oracle::price_update_request` / `pyth_rule::set_price_as_primary` /
`confirm_price_update_request` calls appear** — see §7.

## 5. VERIFIED — canonical object & package IDs
| Role | ID | mut |
|---|---|---|
| Wormhole `State` | `0xaeab97f96cf9877fee2883315d459552b2b921edc16d7ceac6eab944dd88919c` | no |
| Wormhole pkg (`vaa`) | `0x5306f64e312b581766351c07af79c72fcb1cd25147157fdc2f8ad76de9a3fb6a` | – |
| Pyth `State` | `0x1f9310238ee9298fb703c3419030b35b22bb1cc37113e3bb5007c99aec79e5b8` | no |
| Pyth pkg (`pyth`,`hot_potato_vector`) | `0x04e20ddf36af412a4096f9014f4a565af9e812db9a05cc40254846cf6ed0ad91` | – |
| Scallop `Version` | `0x07871c4b3c847a0f674510d4978d5cf6f960452795e8ff6f189fd2088a3f6ac7` | no |
| Scallop `Market` | `0xa757975255146dc9686aa823b7838b507f315d704f428cbadad2f4ea061939d9` | yes |
| `CoinDecimalsRegistry` | `0x200abe9bf19751cc566ae35aa58e2b7e4ff688fc1130f8d8909ea09bc137d668` | no |
| `XOracle` | `0x93d5bf0936b71eb27255941e532fac33b5a5c7759e377b4923af0a1359ad494f` | no |
| Scallop `protocol` pkg | `0xefe8b36d5b2e43728cc323298626b83177803521d195cfb11e15b910e892fddf` | – |
| Cetus `GlobalConfig` | `0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f` | no |

(`PriceInfoObject` ids are per-feed; tx `AYdhgWMq…` updated one feed:
`0x5dec622733a204ca27f5a90d8c2fad453cc6665186fd5dff13a83d0b6c9027ab`.)

## 6. Reconciliation with current code
| Area | Status |
|---|---|
| `decode_obligation` | ✅ **Fixed & verified** against the real object (§2). |
| `liquidate` arg order / returns | Matches §1 — confirm the executor passes args in this exact order + binds both return coins. |
| Object IDs | Put the §5 verified ids in `.env` (several config fields are currently `0x0`). |
| **Pyth refresh approach** | ✅ **Fixed.** `ptb.rs::build_liquidation` now emits the §4 flow (`vaa::parse_and_verify` → `create_authenticated_price_infos_using_accumulator` → per-feed `update_single_price_feed` → `hot_potato_vector::destroy`); the old x_oracle/pyth_rule splice is removed. The executor fetches the Hermes accumulator + extracts the VAA (`oracle::fetch_pyth_accumulator` / `extract_vaa_from_accumulator`). **Proven** by the structural test `live_tests::liquidation_ptb_matches_verified_onchain_shape` (asserts the assembled PTB's MoveCall sequence + arities equal §3). |
| Swap-back | Real tx uses Cetus `pool::flash_swap`/`repay_flash_swap` to convert seized collateral → debt. Our builder uses the existing `cetus_adapter::swap_exact_in_*` (functionally equivalent single swap). Confirm settle/repay against §3 [8..17] in the live dry-run. |

## 7. REMAINING for production (not yet verified)
- **Pyth-accumulator PTB assembly** — ✅ **implemented** (§6) and proven structurally
  (`live_tests`). The only assumption left in the byte path is the PNAU→VAA parse, which is
  unit-tested (`oracle::extract_vaa_parses_pnau_header`) and was confirmed against the real
  tx's bytes (VAA at offset 10, len 952).
- **Config** — set the per-asset `ARB_PYTH_FEED_IDS` (`<coin>=<feed_id>`) and
  `ARB_PYTH_PRICE_INFO_OBJECTS` (`<coin>=<object_id>`) for every debt/collateral you trade;
  Wormhole/Pyth state + package ids default to the verified mainnet values (§5).
- **Which feeds to update** — tx `AYdhgWMq…` refreshed a single feed; our builder refreshes
  **both** the debt + collateral feeds (safe superset). Confirm in the dry-run whether
  fewer suffice (cheaper) — not required for correctness.
- **`XOracle`/`PriceInfoObject` relationship** — empirically, updating the Pyth
  `PriceInfoObject`(s) in-tx is sufficient for `liquidate`'s `&XOracle` read; the exact
  internal linkage was not traced. Treat §3 as the contract.
- **Live dry-run / replay** — the final gate; see §8. This is the only step that still
  needs live state and cannot be done offline.

## 8. On "replay testing" — the hard limit
Sui **cannot** re-execute a historical transaction under a new sender, and a `dry_run`
only succeeds against a **currently-underwater** obligation with **fresh** Hermes
accumulator bytes. So a true "this PTB would have succeeded" replay is not possible
offline. The achievable proofs, in order of strength:
1. **Decode** — ✅ done: end-to-end against the live object (`decode_real_mainnet_obligation`).
2. **Structural equivalence** — ✅ done: `live_tests::liquidation_ptb_matches_verified_onchain_shape`
   asserts the assembled PTB's MoveCall sequence equals §3 and
   `liquidate_and_pyth_calls_have_verified_arity` checks each call's arg/type-arg counts.
3. **Live dry-run** — ⏳ the final gate (operator-run), now **automated** by the
   `liq-validate` binary (`offchain/src/bin/liq_validate.rs` →
   `liquidation::validator::run`). Build with `cargo run --features liq-validate --bin
   liq-validate`: it watches the obligation index, and for each underwater position fetches
   live Hermes bytes, builds the production PTB, and `dry_run`s it — logging
   detected → health → oracle → PTB build → dry-run success/failure → predicted-vs-simulated
   net. It **never submits** (independent of `ARB_SUBMIT_ENABLED`). A green dry-run here is
   the go signal to enable live submission. Requires `ARB_TRACKED_POOLS` to include each
   collateral→debt swap-back pool, plus `ARB_LIQ_ASSETS` / `ARB_PYTH_FEED_IDS` /
   `ARB_PYTH_PRICE_INFO_OBJECTS` for the assets in play.
