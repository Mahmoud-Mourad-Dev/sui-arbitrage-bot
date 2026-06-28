//! Liquidation as a first-class [`OpportunitySource`] (offline).
//!
//! This is the integration seam: the liquidation strategy is *not* a separate bot. It
//! owns no protocol logic itself — it reads the shared obligation index + per-asset params
//! (both kept fresh by background tasks, exactly like pool ingestion keeps the pool cache
//! fresh) and prices swap-backs off the same cached pools every other source sees. The
//! `Opportunity`s it yields flow through the one shared pipeline (dry-run → `RiskGuard` →
//! submit) and the one executor — no executor change, no second risk guard.
//!
//! Adding another lending protocol later means feeding its obligations into the index and
//! its asset params into the map; this source and the executor are untouched.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::liquidation::detect::{self, LiqParams};
use crate::liquidation::types::AssetParams;
use crate::liquidation::ObligationIndex;
use crate::scanner::Opportunity;
use crate::strategy::OpportunitySource;
use crate::types::{PoolState, TokenId};

/// Shared, background-refreshed per-coin risk params (live price + thresholds). The live
/// path refills this from the protocol oracle + market config; tests inject a static map.
/// Empty ⇒ the source never emits (we never act without prices).
pub type SharedParams = Arc<RwLock<HashMap<TokenId, AssetParams>>>;

/// Static per-asset metadata (everything except the live price): decimals + the protocol's
/// liquidation threshold and borrow weight. Parsed from config (`ARB_LIQ_ASSETS`) and
/// combined with a live oracle price into [`AssetParams`].
#[derive(Clone, Debug, PartialEq)]
pub struct AssetMeta {
    pub coin_type: TokenId,
    pub decimals: u8,
    pub liquidation_threshold: f64,
    pub borrow_weight: f64,
}

impl AssetMeta {
    /// Combine static metadata with a live price into full [`AssetParams`].
    #[must_use]
    pub fn with_price(&self, price_usd: f64) -> AssetParams {
        AssetParams {
            coin_type: self.coin_type.clone(),
            decimals: self.decimals,
            price_usd,
            liquidation_threshold: self.liquidation_threshold,
            borrow_weight: self.borrow_weight,
        }
    }
}

/// Parse `ARB_LIQ_ASSETS` entries of the form
/// `"<coin_type>=<decimals>:<liq_threshold>:<borrow_weight>"`. Malformed entries are
/// skipped (the caller can compare the returned length against the input to detect drops).
#[must_use]
pub fn parse_asset_meta(entries: &[String]) -> Vec<AssetMeta> {
    entries
        .iter()
        .filter_map(|e| {
            let (coin, rest) = e.split_once('=')?;
            let mut parts = rest.split(':');
            let decimals = parts.next()?.trim().parse().ok()?;
            let liquidation_threshold = parts.next()?.trim().parse().ok()?;
            let borrow_weight = parts.next()?.trim().parse().ok()?;
            Some(AssetMeta {
                coin_type: coin.trim().to_string(),
                decimals,
                liquidation_threshold,
                borrow_weight,
            })
        })
        .collect()
}

/// The liquidation strategy as an [`OpportunitySource`].
pub struct LiquidationSource {
    index: ObligationIndex,
    params: SharedParams,
    lp: LiqParams,
    health_margin: f64,
}

impl LiquidationSource {
    #[must_use]
    pub fn new(
        index: ObligationIndex,
        params: SharedParams,
        lp: LiqParams,
        health_margin: f64,
    ) -> Self {
        Self {
            index,
            params,
            lp,
            health_margin,
        }
    }
}

impl OpportunitySource for LiquidationSource {
    fn name(&self) -> &str {
        "liquidation"
    }

    fn scan(&self, pools: &[PoolState]) -> Vec<Opportunity> {
        let params = self.params.read().expect("liq params poisoned");
        if params.is_empty() {
            return Vec::new(); // no prices yet ⇒ never act (safe)
        }
        let index = self.index.read().expect("obligation index poisoned");
        index
            .values()
            .filter_map(|ob| {
                detect::detect_obligation(ob, &params, pools, &self.lp, self.health_margin)
            })
            .collect()
    }
}

/// Live params refresh (feature = "live").
///
/// Reads each configured asset's price from the protocol's OWN oracle (the same value
/// `liquidate` will see) and combines it with the static [`AssetMeta`] into [`AssetParams`].
/// Isolated here so the offline source has no SDK dependency.
///
/// VERIFICATION STATUS (production): the price decode is `oracle::scallop_price`
/// (`devInspect` of `price::get_price`, documented there). Confirm against a live market
/// before enabling submit. Defensive: a coin whose price read fails is omitted, and the
/// map is only replaced when at least one price resolved — so the source simply stays
/// inert until prices are available.
#[cfg(feature = "live")]
pub async fn refresh_params(
    client: &sui_sdk::SuiClient,
    config: &crate::config::Config,
    metas: &[AssetMeta],
    params: &SharedParams,
) -> anyhow::Result<()> {
    use anyhow::anyhow;
    use sui_json_rpc_types::SuiObjectDataOptions;
    use sui_types::object::Owner;

    use crate::liquidation::oracle;

    // x_oracle initial shared version (once per refresh; cheap, and it never changes).
    let resp = client
        .read_api()
        .get_object_with_options(
            config.scallop_x_oracle_id.parse()?,
            SuiObjectDataOptions::new().with_owner(),
        )
        .await?;
    let init_ver = match resp.data.and_then(|d| d.owner) {
        Some(Owner::Shared {
            initial_shared_version,
        }) => initial_shared_version.value(),
        _ => return Err(anyhow!("x_oracle object is not shared")),
    };

    let mut next: HashMap<TokenId, AssetParams> = HashMap::new();
    for m in metas {
        match oracle::scallop_price(
            client,
            &config.scallop_package_id,
            &config.scallop_x_oracle_id,
            init_ver,
            &m.coin_type,
        )
        .await
        {
            Ok(q) => {
                next.insert(m.coin_type.clone(), m.with_price(q.price_usd));
            }
            Err(e) => tracing::debug!(coin = %m.coin_type, "liq price read skipped: {e}"),
        }
    }
    if !next.is_empty() {
        *params.write().expect("liq params poisoned") = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clmm;
    use crate::liquidation::types::{Obligation, Position};
    use crate::scanner::{OppKind, Protocol};
    use crate::types::Dex;

    fn params_map() -> HashMap<TokenId, AssetParams> {
        // COLL and DEBT both $1, 6 decimals. COLL liq-threshold 0.4 (so 2:1 collateral
        // still goes underwater); borrow weight 1.
        let coll = AssetMeta {
            coin_type: "COLL".into(),
            decimals: 6,
            liquidation_threshold: 0.4,
            borrow_weight: 1.0,
        };
        let debt = AssetMeta {
            coin_type: "DEBT".into(),
            decimals: 6,
            liquidation_threshold: 0.8,
            borrow_weight: 1.0,
        };
        HashMap::from([
            ("COLL".to_string(), coll.with_price(1.0)),
            ("DEBT".to_string(), debt.with_price(1.0)),
        ])
    }

    /// Deep 1:1 COLL→DEBT pool so swap-back slippage is negligible (0.30% fee only).
    fn swap_pool() -> PoolState {
        PoolState::clmm(
            "0xCD",
            Dex::Cetus,
            "COLL",
            "DEBT",
            clmm::single_range(clmm::Q64, 1_000_000_000_000_000, 3000),
        )
    }

    fn liq_params() -> LiqParams {
        LiqParams {
            close_factor: 0.5,
            liquidation_bonus: 0.05,
            flash_fee_bps: 0,
            gas_cost: 1_000_000,
            min_profit: 1_000_000,
            candidate_fractions: vec![1.0],
        }
    }

    fn obligation(coll: u64, debt: u64) -> Obligation {
        Obligation {
            id: "0xob".into(),
            protocol: Protocol::Scallop,
            collaterals: vec![Position {
                coin_type: "COLL".into(),
                amount: coll,
            }],
            debts: vec![Position {
                coin_type: "DEBT".into(),
                amount: debt,
            }],
        }
    }

    fn index_with(ob: Obligation) -> ObligationIndex {
        let idx: ObligationIndex = Arc::new(RwLock::new(HashMap::new()));
        idx.write().unwrap().insert(ob.id.clone(), ob);
        idx
    }

    #[test]
    fn parse_asset_meta_roundtrip_and_skips_bad() {
        let entries = vec![
            "0x2::sui::SUI=9:0.7:1.0".to_string(),
            "garbage-no-eq".to_string(),
            "0xx::c::C=6:notafloat:1".to_string(),
            "USDC=6:0.8:1.05".to_string(),
        ];
        let metas = parse_asset_meta(&entries);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].coin_type, "0x2::sui::SUI");
        assert_eq!(metas[0].decimals, 9);
        assert!((metas[1].borrow_weight - 1.05).abs() < 1e-9);
    }

    #[test]
    fn source_emits_liquidation_for_underwater_obligation() {
        // 2000 COLL ($2000) × 0.4 = $800 weighted vs $1000 debt → HF 0.8 → liquidatable.
        let src = LiquidationSource::new(
            index_with(obligation(2_000_000_000, 1_000_000_000)),
            Arc::new(RwLock::new(params_map())),
            liq_params(),
            0.0,
        );
        assert_eq!(src.name(), "liquidation");
        let found = src.scan(&[swap_pool()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, OppKind::Liquidation);
        assert!(found[0].net_profit >= 1_000_000);
        assert!(found[0].liquidation.is_some());
    }

    #[test]
    fn source_silent_for_healthy_obligation() {
        // 10_000 COLL ($10k) × 0.4 = $4000 vs $1000 debt → HF 4 → healthy.
        let src = LiquidationSource::new(
            index_with(obligation(10_000_000_000, 1_000_000_000)),
            Arc::new(RwLock::new(params_map())),
            liq_params(),
            0.0,
        );
        assert!(src.scan(&[swap_pool()]).is_empty());
    }

    #[test]
    fn source_silent_without_prices() {
        // Underwater obligation, but no params loaded yet ⇒ never act.
        let src = LiquidationSource::new(
            index_with(obligation(2_000_000_000, 1_000_000_000)),
            Arc::new(RwLock::new(HashMap::new())),
            liq_params(),
            0.0,
        );
        assert!(src.scan(&[swap_pool()]).is_empty());
    }

    #[test]
    fn source_silent_without_swap_pool() {
        // Underwater + priced, but no COLL→DEBT pool to dump collateral ⇒ no opportunity.
        let src = LiquidationSource::new(
            index_with(obligation(2_000_000_000, 1_000_000_000)),
            Arc::new(RwLock::new(params_map())),
            liq_params(),
            0.0,
        );
        assert!(src.scan(&[]).is_empty());
    }
}
