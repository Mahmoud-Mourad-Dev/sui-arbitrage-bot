//! `liq-validate` — operator harness that dry-runs liquidation opportunities on live state.
//!
//! It watches the obligation index, builds the production liquidation PTB for each underwater
//! position, and `dry_run`s it — **never** signing or submitting. This is the final gate
//! before enabling live liquidation submission (docs/scallop-liquidation-verified.md §8).
//!
//! Run:
//! ```bash
//! cargo run --features liq-validate --bin liq-validate
//! ```
//! Requires `.env` with `SUI_RPC_URL`, `ARB_TRACKED_POOLS` (incl. each collateral→debt
//! swap-back pool), `ARB_LIQ_ASSETS`, `ARB_PYTH_FEED_IDS`, and `ARB_PYTH_PRICE_INFO_OBJECTS`.
//! `ARB_SUBMIT_ENABLED` is irrelevant — this binary never submits.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = arb_scanner::config::Config::from_env()?;
    tracing::info!(rpc = %config.rpc_url, "liq-validate: liquidation dry-run validator (no submit)");
    arb_scanner::liquidation::validator::run(&config).await
}
