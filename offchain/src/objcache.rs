//! Shared-object reference cache (feature = "live").
//!
//! A shared object's `initial_shared_version` is fixed for the object's entire
//! lifetime (it is set once, at creation), and an `ObjectArg::SharedObject` needs only
//! that version — the *current* version is resolved by the validators at execution
//! time. So we can memoize it by id forever: the first lookup hits RPC, every
//! subsequent one is an in-memory map read.
//!
//! This removes the ~5–8 sequential `get_object` round-trips the executor previously
//! made on **every** candidate (the static GlobalConfig / Versioned / lender / version
//! / oracle objects), which dominated the hot path (~0.6–1.0 s). Pool refs are already
//! hot in the `ws` registry; this covers everything else the PTB builder resolves.
//!
//! `mutable` is a per-call-site decision (the same object may be borrowed mutably in
//! one PTB and immutably in another) and is therefore NOT part of the cache key — only
//! the immutable `initial_shared_version` is cached.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, bail, Context, Result};
use sui_json_rpc_types::SuiObjectDataOptions;
use sui_sdk::SuiClient;
use sui_types::base_types::{ObjectID, SequenceNumber};
use sui_types::object::Owner;
use sui_types::transaction::{ObjectArg, SharedObjectMutability};

/// Memoizes shared objects' `initial_shared_version` by id. Cheap to clone-share via
/// `Arc`; safe for concurrent reads/writes.
#[derive(Default)]
pub struct ObjRefCache {
    initial_versions: RwLock<HashMap<ObjectID, SequenceNumber>>,
}

impl ObjRefCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an `ObjectArg::SharedObject` for `id_str`, memoizing its
    /// `initial_shared_version`. First call per id resolves it from chain; later calls
    /// are served from memory.
    pub async fn shared_arg(
        &self,
        client: &SuiClient,
        id_str: &str,
        mutable: bool,
    ) -> Result<ObjectArg> {
        let id: ObjectID = id_str
            .parse()
            .with_context(|| format!("object id {id_str}"))?;
        if let Some(v) = self
            .initial_versions
            .read()
            .expect("objcache poisoned")
            .get(&id)
            .copied()
        {
            return Ok(make_arg(id, v, mutable));
        }
        let v = fetch_initial_shared_version(client, id, id_str).await?;
        self.initial_versions
            .write()
            .expect("objcache poisoned")
            .insert(id, v);
        Ok(make_arg(id, v, mutable))
    }

    /// Number of memoized objects (metrics / tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.initial_versions
            .read()
            .expect("objcache poisoned")
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn make_arg(id: ObjectID, initial_shared_version: SequenceNumber, mutable: bool) -> ObjectArg {
    ObjectArg::SharedObject {
        id,
        initial_shared_version,
        mutability: if mutable {
            SharedObjectMutability::Mutable
        } else {
            SharedObjectMutability::Immutable
        },
    }
}

async fn fetch_initial_shared_version(
    client: &SuiClient,
    id: ObjectID,
    id_str: &str,
) -> Result<SequenceNumber> {
    let resp = client
        .read_api()
        .get_object_with_options(id, SuiObjectDataOptions::new().with_owner())
        .await?;
    let data = resp
        .data
        .ok_or_else(|| anyhow!("object {id_str} not found"))?;
    match data.owner {
        Some(Owner::Shared {
            initial_shared_version,
        }) => Ok(initial_shared_version),
        other => bail!("object {id_str} is not shared: {other:?}"),
    }
}
