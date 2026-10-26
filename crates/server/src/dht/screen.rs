//! The content screen every fetched torrent passes before anything with its name is written
//! (docs/DHT.md §7, D53, D65, D68): the hard illegal-content blocklist of `core::blocklist`.
//! The seam **fails closed**: with no screen installed, verified metadata is dropped from memory
//! and the hash is put back in the queue (`dht_seen` `queued`, `last_error = 'unscreened'`, due
//! again after [`UNSCREENED_RETRY_SECS`]), so no name reaches the database unscreened.
//!
//! The process-wide list is installed once at start by [`install_from_config`] (built-in list
//! plus `MAGNETAR_BLOCKLIST_FILE`, additive only); a list that does not load stops the
//! process before any command runs.

use std::fmt::Debug;
use std::sync::Arc;

use anyhow::Context;
use magnetar_core::blocklist::{self, Blocklist, Tier};

use crate::config::Config;

/// When an unscreened hash is due again (the blocklist may be installed by then).
pub const UNSCREENED_RETRY_SECS: i64 = 6 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Illegal content: only the infohash is kept (`blocked_infohashes`), never a name. The
    /// tier is for counts only.
    Block(Tier),
}

/// A content screen over every name and path variant of an info dict
/// ([`magnetar_core::metainfo::InfoSummary::all_names`], padding paths included).
///
/// Implementations must not log, store or return the names they read.
pub trait Screen: Send + Sync + Debug {
    /// `adult`: the classifier's adult result (category `porn` or an adult signal), which
    /// turns on the context tier.
    fn verdict(&self, names: &[String], adult: bool) -> Verdict;
    /// Recorded in `blocked_infohashes.rule_version`.
    fn rule_version(&self) -> i16;
}

/// The `core::blocklist` screen.
#[derive(Debug, Clone)]
pub struct BlocklistScreen(pub Arc<Blocklist>);

impl Screen for BlocklistScreen {
    fn verdict(&self, names: &[String], adult: bool) -> Verdict {
        let none: [&str; 0] = [];
        match self.0.check(names, &none, adult) {
            blocklist::Verdict::Allow => Verdict::Allow,
            blocklist::Verdict::Block(t) => Verdict::Block(t),
        }
    }
    fn rule_version(&self) -> i16 {
        self.0.version()
    }
}

/// The process-wide list as a screen; None when no list is loaded (then nothing is stored).
pub fn global_screen() -> Option<Arc<dyn Screen>> {
    blocklist::global().map(|b| Arc::new(BlocklistScreen(Arc::new(b.clone()))) as Arc<dyn Screen>)
}

/// Load the built-in list plus `MAGNETAR_BLOCKLIST_FILE` and install it process-wide.
/// Errors (refuse to start, D53) when either does not load. The file's terms are never
/// logged: only how many were added.
pub fn install_from_config(cfg: &Config) -> anyhow::Result<()> {
    let mut list = Blocklist::builtin().context("built-in blocklist")?;
    if let Some(path) = &cfg.blocklist_file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("MAGNETAR_BLOCKLIST_FILE {}", path.display()))?;
        let n = list
            .add_plain(&text)
            .with_context(|| format!("MAGNETAR_BLOCKLIST_FILE {}", path.display()))?;
        tracing::info!("blocklist: {n} extra terms from MAGNETAR_BLOCKLIST_FILE");
    }
    // A second call (tests) keeps the first list.
    let _ = blocklist::install(list);
    anyhow::ensure!(blocklist::global().is_some(), "no blocklist loaded");
    Ok(())
}
