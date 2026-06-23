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
    /// Flash-loan execution: borrow the input capital instead of using owned funds.
    pub flash_enabled: bool,
    /// Provider name resolved by `flashloan::provider_from` (e.g. "mock").
    pub flash_provider: String,
    /// Flash-loan fee in bps, charged on the loan size and netted from profit.
    pub flash_fee_bps: u64,
    /// Shared lender/vault object id passed to borrow/repay.
    pub flash_lender_id: String,
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
            flash_enabled: env_parse("ARB_FLASH_ENABLED", false)?,
            flash_provider: env_or("ARB_FLASH_PROVIDER", "mock"),
            flash_fee_bps: env_parse("ARB_FLASH_FEE_BPS", 0)?,
            flash_lender_id: env_or("ARB_FLASH_LENDER_ID", "0x0"),
        })
    }
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
