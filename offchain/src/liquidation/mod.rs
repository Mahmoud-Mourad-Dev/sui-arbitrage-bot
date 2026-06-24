//! Liquidation opportunity source.
//!
//! A new [`crate::strategy::OpportunitySource`] that emits the existing
//! [`crate::scanner::Opportunity`] (with `kind = Liquidation`) into the one shared
//! pipeline — it is not a separate bot. Layers:
//!   * [`types`]  — obligation / position / asset-param data model (offline).
//!   * [`health`] — stage-1 LOCAL health approximation (offline, over-includes).
//!   * [`detect`] — stage-1 sizing into an `Opportunity` net of swap slippage / fee /
//!     gas (offline). The *authoritative* sizing/verdict is the protocol's own
//!     on-chain read, called in the live path.
//!   * [`index`]  — event-driven obligation index (live).
//!   * [`oracle`] — protocol oracle (Pyth/x_oracle) prices + freshness (live).

pub mod detect;
pub mod health;
pub mod types;

#[cfg(feature = "live")]
pub mod index;
#[cfg(feature = "live")]
pub mod oracle;
