//! Thread-safe local reserve cache.
//!
//! The WebSocket task writes pool updates here; the scanner reads consistent
//! snapshots. A single `RwLock<HashMap>` is enough at this scale (hundreds of
//! pools); swap to a sharded map (`dashmap`) only if write contention shows up.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::types::{PoolId, PoolState};

#[derive(Default)]
pub struct ReserveCache {
    pools: RwLock<HashMap<PoolId, PoolState>>,
}

impl ReserveCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a pool snapshot (called on every WS update).
    pub fn upsert(&self, pool: PoolState) {
        self.pools
            .write()
            .expect("reserve cache poisoned")
            .insert(pool.id.clone(), pool);
    }

    /// Update just the reserves of an existing V2 pool. Returns `false` if the pool
    /// is unknown or not a V2 pool (CLMM pools are refreshed via `upsert` of a freshly
    /// decoded snapshot — see `ws.rs`).
    pub fn update_reserves(&self, id: &str, reserve_a: u64, reserve_b: u64) -> bool {
        use crate::types::PoolKind;
        let mut guard = self.pools.write().expect("reserve cache poisoned");
        match guard.get_mut(id) {
            Some(p) => match &mut p.kind {
                PoolKind::V2 {
                    reserve_a: ra,
                    reserve_b: rb,
                    ..
                } => {
                    *ra = reserve_a;
                    *rb = reserve_b;
                    true
                }
                PoolKind::Clmm(_) => false,
            },
            None => false,
        }
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<PoolState> {
        self.pools
            .read()
            .expect("reserve cache poisoned")
            .get(id)
            .cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pools.read().expect("reserve cache poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A consistent snapshot of every pool, for the scanner to work on without
    /// holding the lock across the (relatively expensive) cycle search.
    #[must_use]
    pub fn snapshot(&self) -> Vec<PoolState> {
        self.pools
            .read()
            .expect("reserve cache poisoned")
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dex;

    fn pool(id: &str) -> PoolState {
        PoolState::v2(id, Dex::AmmV2, "A", "B", 1_000, 1_000, 30)
    }

    #[test]
    fn upsert_get_update() {
        use crate::types::PoolKind;
        let c = ReserveCache::new();
        assert!(c.is_empty());
        c.upsert(pool("0x1"));
        assert_eq!(c.len(), 1);
        assert!(c.update_reserves("0x1", 2_000, 500));
        let p = c.get("0x1").unwrap();
        assert!(matches!(
            p.kind,
            PoolKind::V2 {
                reserve_a: 2_000,
                reserve_b: 500,
                ..
            }
        ));
        assert!(!c.update_reserves("0xUNKNOWN", 1, 1));
    }
}
