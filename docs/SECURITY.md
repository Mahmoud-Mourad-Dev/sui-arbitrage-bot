# Security Review Checklist

Threat model: an adversary controls the mempool ordering, can run competing bots,
can move pool prices between our simulation and execution, and may deploy
malicious "pools." Our funds must never be at risk beyond gas on a failed attempt.

## On-chain (Move)

- [x] **Profit gate is mandatory.** `ArbReceipt` is a hot potato → `settle` (and
      its profit assertion) cannot be skipped. The check uses the *real* recorded
      input value, not a caller-supplied number.
- [x] **Overflow-safe profit target.** `assert_profit` guards
      `initial + min_profit` against u64 wraparound before comparing.
- [x] **All swap math in u128.** `get_amount_out` / `get_amount_in` cannot
      overflow for u64 reserves/amounts; degenerate inputs abort.
- [x] **Per-hop slippage floor.** Adapters enforce `min_out`; the executor
      enforces end-to-end profit. Defense in depth.
- [x] **Privileged actions gated by `AdminCap`.** Pool creation/funding require
      the capability; the executor path needs none (permissionless, no admin keys
      on the hot path).
- [x] **No `public entry` foot-guns.** Functions return values for PTB
      composition; no hidden auto-transfer that could be redirected.
- [ ] **Reentrancy.** Move + PTB model has no synchronous reentrancy; still, when
      wiring Cetus `flash_swap`, ensure the flash receipt is repaid in-block and
      never stored.
- [ ] **Malicious adapter / pool.** Only call audited venue packages pinned by
      rev in `Move.toml`. An attacker-controlled "pool" can return less than
      `min_out` → the swap or `settle` aborts; it can never *take* extra funds
      because we pass an exact input coin and receive a typed output coin.
- [ ] **Package upgrades.** Decide upgrade policy before mainnet (immutable vs.
      `UpgradeCap` held in multisig). Pin dependency revs.

## Off-chain (Rust)

- [x] **Submit-only-if-profitable.** Dry-run gates submission; `min_profit`
      covers gas. Stale-cache mistakes cost only gas (settle aborts).
- [x] **No panics on the hot path.** AMM math returns `Option`; scanner skips bad
      hops instead of crashing.
- [ ] **Key management.** Signing key loaded from an OS keystore / env, never
      committed (`.env`, `*.keystore`, `*.key` are gitignored). Prefer a dedicated
      hot wallet funded with only working capital.
- [ ] **RPC trust.** A lying RPC could feed bad reserves → still bounded by the
      on-chain `settle` gate, but use a trusted/own fullnode for the WS feed.
- [ ] **MEV / frontrunning.** Assume our tx can be observed. Keep `min_profit`
      above the cost of being sandwiched on the first hop; consider private
      submission if/when available on Sui.
- [ ] **Rate / spend limits.** Cap per-tx input and per-hour spend so a logic bug
      can't drain the hot wallet.
- [ ] **Dependency pinning.** `Cargo.lock` committed; `sui-sdk` git deps pinned to
      a known rev per network.

## Operational

- [ ] Separate hot wallet (working capital only) from treasury.
- [ ] Alerting on abnormal `settle` abort rate or gas spend.
- [ ] Kill switch: env flag / config that stops submission instantly.
- [ ] Run `sui move test` + `cargo test` + `cargo clippy -D warnings` in CI on
      every change before deploy.
```
