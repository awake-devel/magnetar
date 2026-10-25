//! ntfy notifications + saved-search evaluation (D25). Owner: trust-sync agent.

use crate::model::{SavedSearch, TorrentSummary};
use crate::store::Store;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NtfyConfig {
    pub base_url: String,
    pub topic: String,
    pub token: Option<String>,
}

/// JSON publish to the root URL (termux.md §4). `click` e.g. a magnet or magnetar:// link.
pub async fn ntfy_publish(
    cfg: &NtfyConfig,
    title: &str,
    message: &str,
    click: Option<&str>,
) -> crate::Result<()> {
    let _ = (cfg, title, message, click);
    todo!("notify::ntfy_publish")
}

/// Run each saved search with notify=true; return hits newer than `last_seen_created_at`
/// (first run: record the newest and return nothing), and persist the new watermark.
pub async fn poll_saved_searches<S: Store + ?Sized>(
    store: &S,
    now: i64,
) -> crate::Result<Vec<(SavedSearch, Vec<TorrentSummary>)>> {
    let _ = (store, now);
    todo!("notify::poll_saved_searches")
}
