//! Phone → server sync (D13–D15, ARCHITECTURE.md §7). Owner: trust-sync agent.

use crate::api::ApiClient;
use crate::model::RawEvent;

pub const FRAME_SIZE_LIMIT: u64 = 60_000;
pub const SYNC_KINDS: &[u16] = &[2003, 2004, 1111];

#[async_trait::async_trait]
pub trait SyncSource: Send + Sync {
    /// (created_at, id) for every event ever stored locally (sync_ids; includes evicted bodies).
    async fn neg_items(&self) -> crate::Result<Vec<(u64, [u8; 32])>>;
    async fn load_events(&self, ids: &[String]) -> crate::Result<Vec<RawEvent>>;
    async fn unsynced(&self, limit: usize) -> crate::Result<Vec<RawEvent>>;
    async fn mark_synced(&self, ids: &[String]) -> crate::Result<()>;
    async fn record_failure(&self, id: &str, reason: &str) -> crate::Result<()>;
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SyncReport {
    /// "negentropy" | "batch" | "none"
    pub method: String,
    pub have: u64,
    pub uploaded: u64,
    pub duplicates: u64,
    pub failed: u64,
    pub need_ignored: u64,
    pub error: Option<String>,
}

/// Negentropy Up-sync over `relay_ws_url` (e.g. ws://127.0.0.1:8787/relay?token=..), falling back to
/// `api.post_batch` with `unsynced()`. Marks synced only on OK true (incl. duplicate:).
pub async fn sync_up<S: SyncSource + ?Sized>(
    src: &S,
    relay_ws_url: &str,
    api: &ApiClient,
) -> crate::Result<SyncReport> {
    let _ = (src, relay_ws_url, api);
    todo!("sync::sync_up")
}

/// Server side: answer one NEG-OPEN/NEG-MSG. Used by the server relay endpoint.
pub struct NegResponder {
    // negentropy::Negentropy<'static, NegentropyStorageVector>
    inner: negentropy::Negentropy<'static, negentropy::NegentropyStorageVector>,
}

impl NegResponder {
    /// Build from (created_at, id) items; errors on bad frame limit.
    pub fn new(items: Vec<(u64, [u8; 32])>) -> crate::Result<Self> {
        let _ = items;
        todo!("sync::NegResponder::new")
    }
    /// Hex in, hex out.
    pub fn reconcile_hex(&mut self, msg_hex: &str) -> crate::Result<String> {
        let _ = (msg_hex, &mut self.inner);
        todo!()
    }
}
