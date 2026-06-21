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

    /// Update just the reserves of an existing pool. Returns `false` if unknown.
    pub fn update_reserves(&self, id: &str, reserve_a: u64, reserve_b: u64) -> bool {
        let mut guard = self.pools.write().expect("reserve cache poisoned");
        match guard.get_mut(id) {
            Some(p) => {
                p.reserve_a = reserve_a;
                p.reserve_b = reserve_b;
                true
            }
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
        PoolState {
            id: id.to_string(),
            dex: Dex::AmmV2,
            token_a: "A".into(),
            token_b: "B".into(),
            reserve_a: 1_000,
            reserve_b: 1_000,
            fee_bps: 30,
        }
    }

    #[test]
    fn upsert_get_update() {
        let c = ReserveCache::new();
        assert!(c.is_empty());
        c.upsert(pool("0x1"));
        assert_eq!(c.len(), 1);
        assert!(c.update_reserves("0x1", 2_000, 500));
        let p = c.get("0x1").unwrap();
        assert_eq!((p.reserve_a, p.reserve_b), (2_000, 500));
        assert!(!c.update_reserves("0xUNKNOWN", 1, 1));
    }
}
