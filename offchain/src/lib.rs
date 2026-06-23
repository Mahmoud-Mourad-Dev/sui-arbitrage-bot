//! Off-chain arbitrage engine for the `arbitrage_system` Sui package.
//!
//! Layers:
//! - [`amm`]     constant-product math, bit-for-bit identical to `math.move`.
//! - [`types`]   pool/route data model.
//! - [`cache`]   thread-safe local reserve cache.
//! - [`scanner`] cycle detection + trade sizing over the cached graph.
//! - [`config`]  runtime configuration from env.
//!
//! Live (feature = "live") layers talk to the chain:
//! - [`ws`]       WebSocket ingestion of pool updates into the cache.
//! - [`ptb`]      Programmable Transaction Block construction for a route.
//! - [`executor`] dry-run + submit only profitable PTBs.

pub mod amm;
pub mod cache;
pub mod clmm;
pub mod config;
pub mod flashloan;
pub mod ptb;
pub mod scanner;
pub mod types;

#[cfg(feature = "live")]
pub mod executor;
#[cfg(feature = "live")]
pub mod ws;
