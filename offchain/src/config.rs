//! Runtime configuration, loaded from environment variables with sane testnet
//! defaults. See `.env.example`.

use std::env;
use std::str::FromStr;

use anyhow::{Context, Result};

use crate::types::TokenId;

#[derive(Clone, Debug)]
pub struct Config {
    pub rpc_url: String,
    pub ws_url: String,
    /// Published `arbitrage_system` package id.
    pub package_id: String,
    /// Base token every cycle starts and ends in.
    pub base_token: TokenId,
    /// Minimum net profit (MIST) to bother submitting.
    pub min_profit: u64,
    /// Gas budget per PTB (MIST).
    pub gas_budget: u64,
    /// Estimated gas cost charged against gross profit when sizing trades (MIST).
    pub gas_cost_estimate: u64,
    /// Max cycle length to search (3 covers triangular arbitrage).
    pub max_hops: usize,
    /// Candidate input sizes (MIST) tried per cycle.
    pub candidate_inputs: Vec<u64>,
    /// Re-scan cadence when not event-driven (ms).
    pub poll_interval_ms: u64,
    /// Pool ingestion mode: `"poll"` (re-read every tracked pool each interval) or
    /// `"checkpoint"` (watch the checkpoint tip and refresh only when the chain advances,
    /// decoding only pools whose object version changed). `"checkpoint"` is the seam for a
    /// co-located fullnode stream; over a remote RPC it still cuts redundant reads/work.
    pub ingest_mode: String,
    /// Pools to track, as `"<dex>:<object_id>"` (e.g. `cetus:0x..,turbos:0x..`). Coin
    /// types + fee tier are derived from each pool's on-chain object type.
    pub tracked_pools: Vec<String>,
    /// Flash-loan execution: borrow the input capital instead of using owned funds.
    pub flash_enabled: bool,
    /// Provider name resolved by `flashloan::provider_from` (e.g. "mock").
    pub flash_provider: String,
    /// Flash-loan fee in bps, charged on the loan size and netted from profit.
    pub flash_fee_bps: u64,
    /// Shared lender/vault object id passed to borrow/repay (Scallop: the `Market`).
    pub flash_lender_id: String,
    /// Provider's secondary shared object id when it needs one (Scallop: `Version`).
    pub flash_version_id: String,
    /// Pool object ids to never route through (broken/empty pools, e.g. the report's
    /// Momentum USDB/USDC pool that quotes ~0).
    pub pool_blacklist: Vec<String>,
    /// Hard halt — when true the executor submits nothing.
    pub kill_switch: bool,
    /// Halt submitting once realized daily P&L drops below `-this` (USD). 0 disables.
    pub max_daily_loss_usd: f64,
    /// Master switch for actually signing + submitting transactions. **Off by
    /// default**: the bot detects, authoritatively re-prices, and dry-runs, but does
    /// not `execute_transaction_block` until this is explicitly enabled.
    pub submit_enabled: bool,
    /// Max age (ms) of the authoritative quote before submit; re-quote if older.
    pub max_quote_age_ms: u64,
    /// Per-hop slippage floor (bps) applied to dry-run outputs to set `min_out`, so a
    /// stale route fails fast/cheap on-chain instead of landing a bad fill.
    pub per_hop_slippage_bps: u64,
    /// Enable the liquidation opportunity source (index + detect; paper unless submit on).
    pub liq_enabled: bool,
    /// Max fraction of debt repayable per liquidation call (close factor, e.g. 0.5).
    pub liq_close_factor: f64,
    /// Liquidation bonus / collateral discount (e.g. 0.05 = seize 5% extra value).
    pub liq_bonus: f64,
    /// Health pre-filter margin: shortlist obligations with HF < 1 + this (over-include;
    /// the on-chain read makes the final call).
    pub liq_health_margin: f64,
    /// Per-asset liquidation metadata, as `"<coin_type>=<decimals>:<liq_threshold>:<borrow_weight>"`
    /// entries. Live prices come from the protocol oracle; this supplies the rest.
    pub liq_assets: Vec<String>,
    /// Scallop shared object ids the liquidate call needs (Market == `flash_lender_id`,
    /// Version == `flash_version_id`); these add the oracle + decimals registry.
    pub scallop_x_oracle_id: String,
    pub scallop_registry_id: String,
    /// Scallop `protocol` package id (flash_loan + liquidate move calls).
    pub scallop_package_id: String,
    /// Signing: path to the Sui file keystore + the sender address to use.
    /// The key never leaves the keystore and is never logged.
    pub keystore_path: String,
    pub sender_address: String,
    /// Venue shared-object ids the adapters need (versions resolved fresh on-chain).
    pub cetus_global_config_id: String,
    pub turbos_versioned_id: String,
    /// Liquidation in-PTB oracle refresh (Scallop x_oracle pyth_rule). Object ids;
    /// versions resolved fresh on-chain. See docs/testnet-runbook.md.
    pub x_oracle_package_id: String,
    pub pyth_rule_package_id: String,
    pub pyth_state_id: String,
    pub scallop_pyth_registry_id: String,
    /// Per-coin Pyth `PriceInfoObject` ids, as `"<coin_type>=<object_id>"` entries.
    pub pyth_price_info_objects: Vec<String>,
    /// Per-coin Pyth feed ids (hex), as `"<coin_type>=<feed_id>"` entries — used to fetch
    /// the Hermes accumulator for the liquidation's debt + collateral assets.
    pub pyth_feed_ids: Vec<String>,
    /// Wormhole `State` object + package (verified flow: `vaa::parse_and_verify`).
    pub wormhole_state_id: String,
    pub wormhole_package_id: String,
    /// Pyth package (verified flow: `pyth` + `hot_potato_vector`).
    pub pyth_package_id: String,
    /// Hermes endpoint for Pyth price updates.
    pub hermes_url: String,
    /// MIST paid per `update_single_price_feed` call (split off the gas coin).
    pub pyth_fee: u64,
    /// `host:port` for the Prometheus `/metrics` endpoint. Empty disables it.
    pub metrics_addr: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            rpc_url: env_or("SUI_RPC_URL", "https://fullnode.testnet.sui.io:443"),
            ws_url: env_or("SUI_WS_URL", "wss://fullnode.testnet.sui.io:443"),
            package_id: env_or("ARB_PACKAGE_ID", "0x0"),
            base_token: env_or("ARB_BASE_TOKEN", "0x2::sui::SUI"),
            min_profit: env_parse("ARB_MIN_PROFIT", 10_000_000)?,
            gas_budget: env_parse("ARB_GAS_BUDGET", 20_000_000)?,
            gas_cost_estimate: env_parse("ARB_GAS_COST", 8_000_000)?,
            max_hops: env_parse("ARB_MAX_HOPS", 3)?,
            candidate_inputs: env_inputs(),
            poll_interval_ms: env_parse("ARB_POLL_MS", 500)?,
            ingest_mode: env_or("ARB_INGEST_MODE", "poll"),
            tracked_pools: env_list("ARB_TRACKED_POOLS"),
            flash_enabled: env_parse("ARB_FLASH_ENABLED", false)?,
            flash_provider: env_or("ARB_FLASH_PROVIDER", "mock"),
            flash_fee_bps: env_parse("ARB_FLASH_FEE_BPS", 0)?,
            flash_lender_id: env_or("ARB_FLASH_LENDER_ID", "0x0"),
            flash_version_id: env_or("ARB_FLASH_VERSION_ID", "0x0"),
            pool_blacklist: env_list("ARB_POOL_BLACKLIST"),
            kill_switch: env_parse("ARB_KILL_SWITCH", false)?,
            max_daily_loss_usd: env_parse("ARB_MAX_DAILY_LOSS_USD", 0.0)?,
            submit_enabled: env_parse("ARB_SUBMIT_ENABLED", false)?,
            max_quote_age_ms: env_parse("ARB_MAX_QUOTE_AGE_MS", 1_500)?,
            per_hop_slippage_bps: env_parse("ARB_PER_HOP_SLIPPAGE_BPS", 30)?,
            liq_enabled: env_parse("ARB_LIQ_ENABLED", false)?,
            liq_close_factor: env_parse("ARB_LIQ_CLOSE_FACTOR", 0.5)?,
            liq_bonus: env_parse("ARB_LIQ_BONUS", 0.05)?,
            liq_health_margin: env_parse("ARB_LIQ_HEALTH_MARGIN", 0.02)?,
            liq_assets: env_list("ARB_LIQ_ASSETS"),
            scallop_x_oracle_id: env_or("ARB_SCALLOP_X_ORACLE_ID", "0x0"),
            scallop_registry_id: env_or("ARB_SCALLOP_REGISTRY_ID", "0x0"),
            scallop_package_id: env_or(
                "ARB_SCALLOP_PACKAGE_ID",
                "0xefe8b36d5b2e43728cc323298626b83177803521d195cfb11e15b910e892fddf",
            ),
            keystore_path: env_or("ARB_KEYSTORE_PATH", ""),
            sender_address: env_or("ARB_SENDER_ADDRESS", "0x0"),
            cetus_global_config_id: env_or("ARB_CETUS_GLOBAL_CONFIG_ID", "0x0"),
            turbos_versioned_id: env_or("ARB_TURBOS_VERSIONED_ID", "0x0"),
            x_oracle_package_id: env_or("ARB_X_ORACLE_PACKAGE_ID", "0x0"),
            pyth_rule_package_id: env_or(
                "ARB_PYTH_RULE_PACKAGE_ID",
                "0x1cf913c825c202cbbb71c378edccb9c04723fa07a73b88677b2ef89c6e203a85",
            ),
            pyth_state_id: env_or("ARB_PYTH_STATE_ID", "0x0"),
            scallop_pyth_registry_id: env_or("ARB_SCALLOP_PYTH_REGISTRY_ID", "0x0"),
            pyth_price_info_objects: env_list("ARB_PYTH_PRICE_INFO_OBJECTS"),
            pyth_feed_ids: env_list("ARB_PYTH_FEED_IDS"),
            wormhole_state_id: env_or(
                "ARB_WORMHOLE_STATE_ID",
                "0xaeab97f96cf9877fee2883315d459552b2b921edc16d7ceac6eab944dd88919c",
            ),
            wormhole_package_id: env_or(
                "ARB_WORMHOLE_PACKAGE_ID",
                "0x5306f64e312b581766351c07af79c72fcb1cd25147157fdc2f8ad76de9a3fb6a",
            ),
            pyth_package_id: env_or(
                "ARB_PYTH_PACKAGE_ID",
                "0x04e20ddf36af412a4096f9014f4a565af9e812db9a05cc40254846cf6ed0ad91",
            ),
            hermes_url: env_or("ARB_HERMES_URL", "https://hermes.pyth.network"),
            pyth_fee: env_parse("ARB_PYTH_FEE", 1)?,
            metrics_addr: env_or("ARB_METRICS_ADDR", "0.0.0.0:9100"),
        })
    }

    /// The Pyth `PriceInfoObject` id configured for `coin_type`, from
    /// `pyth_price_info_objects` (`"<coin_type>=<object_id>"`). `None` if unset.
    #[must_use]
    pub fn price_info_object(&self, coin_type: &str) -> Option<&str> {
        self.pyth_price_info_objects.iter().find_map(|e| {
            let (k, v) = e.split_once('=')?;
            (k.trim() == coin_type).then_some(v.trim())
        })
    }

    /// The Pyth feed id (hex) configured for `coin_type`, from `pyth_feed_ids`. `None` if unset.
    #[must_use]
    pub fn pyth_feed_id(&self, coin_type: &str) -> Option<&str> {
        self.pyth_feed_ids.iter().find_map(|e| {
            let (k, v) = e.split_once('=')?;
            (k.trim() == coin_type).then_some(v.trim())
        })
    }
}

/// Comma-separated env list (e.g. pool ids); empty/absent → empty vec.
fn env_list(key: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T: FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match env::var(key) {
        Ok(v) => v
            .parse::<T>()
            .with_context(|| format!("invalid value for {key}")),
        Err(_) => Ok(default),
    }
}

/// `ARB_CANDIDATE_INPUTS` = comma-separated MIST values; otherwise a geometric grid.
fn env_inputs() -> Vec<u64> {
    if let Ok(v) = env::var("ARB_CANDIDATE_INPUTS") {
        let parsed: Vec<u64> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    // 0.1, 0.5, 1, 5, 10, 50, 100, 500 SUI (in MIST). Capped by the $150 budget.
    vec![
        100_000_000,
        500_000_000,
        1_000_000_000,
        5_000_000_000,
        10_000_000_000,
        50_000_000_000,
        100_000_000_000,
        500_000_000_000,
    ]
}
