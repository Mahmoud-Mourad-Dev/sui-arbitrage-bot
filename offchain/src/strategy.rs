//! Opportunity-source abstraction.
//!
//! Every strategy — cross-venue arbitrage, liquidation, backrun-arb — is a *source*
//! that yields the same [`Opportunity`], which then flows through the ONE shared
//! pipeline (authoritative re-quote → dry-run → `RiskGuard` → submit). Strategies are
//! sources, **not** separate bots: there is a single executor, a single profit gate,
//! a single risk guard. This is the "reuse, do not fork" rule made concrete.
//!
//! Two source flavors, both converging on `Opportunity`:
//!   * [`OpportunitySource`] — synchronous, prices off the cached pool graph
//!     (arbitrage, backrun). Implemented here by [`ArbSource`].
//!   * Live sources (liquidation) detect from a local index + authoritative
//!     `devInspect` reads; they are async and live-feature-gated, but emit the same
//!     `Opportunity` into the same executor. See `offchain/src/liquidation/`.

use crate::scanner::{self, OppKind, Opportunity, ScanParams};
use crate::types::PoolState;

/// A cache-graph opportunity source (arbitrage, backrun). Given the current pool
/// snapshot, produce zero or more ready opportunities.
pub trait OpportunitySource {
    fn name(&self) -> &str;
    fn scan(&self, pools: &[PoolState]) -> Vec<Opportunity>;
}

/// The existing cross-venue arbitrage scanner, as a source.
pub struct ArbSource {
    pub params: ScanParams,
}

impl ArbSource {
    #[must_use]
    pub fn new(params: ScanParams) -> Self {
        Self { params }
    }
}

impl OpportunitySource for ArbSource {
    fn name(&self) -> &str {
        "arb"
    }
    fn scan(&self, pools: &[PoolState]) -> Vec<Opportunity> {
        // The scanner currently returns the single best cycle; a source naturally
        // yields a list, so callers can merge multiple sources before ranking.
        scanner::find_best(pools, &self.params)
            .into_iter()
            .collect()
    }
}

/// Backrun-arb: after a large swap moves a pool, re-run the arb scan on the freshly
/// updated graph. It reuses the entire arb path — the only differences are the trigger
/// (a swap event, driven by the live loop, not this struct) and the `Backrun` tag.
/// High reuse, minimal new code: another source on the one pipeline.
pub struct BackrunSource {
    arb: ArbSource,
}

impl BackrunSource {
    #[must_use]
    pub fn new(params: ScanParams) -> Self {
        Self {
            arb: ArbSource::new(params),
        }
    }
}

impl OpportunitySource for BackrunSource {
    fn name(&self) -> &str {
        "backrun"
    }
    fn scan(&self, pools: &[PoolState]) -> Vec<Opportunity> {
        self.arb
            .scan(pools)
            .into_iter()
            .map(|mut o| {
                o.kind = OppKind::Backrun;
                o
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::OppKind;
    use crate::types::Dex;

    fn pool(id: &str, a: &str, b: &str, ra: u64, rb: u64) -> PoolState {
        PoolState::v2(id, Dex::AmmV2, a, b, ra, rb, 30)
    }

    #[test]
    fn arb_source_yields_arb_opportunities() {
        let pools = vec![
            pool("0xAB", "A", "B", 1_000, 1_000),
            pool("0xBC", "B", "C", 1_000, 1_000),
            pool("0xCA", "C", "A", 1_000, 2_000),
        ];
        let src = ArbSource::new(ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![10],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        });
        let found = src.scan(&pools);
        assert_eq!(src.name(), "arb");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, OppKind::Arb);
    }

    #[test]
    fn arb_source_empty_when_no_edge() {
        let pools = vec![
            pool("0xAB", "A", "B", 1_000_000, 1_000_000),
            pool("0xBC", "B", "C", 1_000_000, 1_000_000),
            pool("0xCA", "C", "A", 1_000_000, 1_000_000),
        ];
        let src = ArbSource::new(ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![1_000],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        });
        assert!(src.scan(&pools).is_empty());
    }

    #[test]
    fn backrun_source_tags_opportunities_as_backrun() {
        let pools = vec![
            pool("0xAB", "A", "B", 1_000, 1_000),
            pool("0xBC", "B", "C", 1_000, 1_000),
            pool("0xCA", "C", "A", 1_000, 2_000),
        ];
        let src = BackrunSource::new(ScanParams {
            base_token: "A".into(),
            max_hops: 3,
            candidate_inputs: vec![10],
            gas_cost: 0,
            flash_fee_bps: 0,
            min_profit: 1,
        });
        let found = src.scan(&pools);
        assert_eq!(src.name(), "backrun");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, OppKind::Backrun); // reuses arb scan, re-tagged
    }
}
