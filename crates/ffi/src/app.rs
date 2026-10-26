//! Shared state behind the `Magnetar` object (store, config, API client, download engine).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;

use magnetar_core::api::{ApiClient, Fetched};
use magnetar_core::crawler::{CrawlConfig, DEFAULT_SEED_RELAYS};
use magnetar_core::model::{SavedSearch, TorrentSummary, parse_pubkey};
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::store::keys;

use crate::convert::StoredConfig;
use crate::store::SqliteStore;
use crate::{FfiConfig, FfiError, FfiSearchQuery};

pub const LAST_CRAWL: &str = "ffi.last_crawl";
pub const LAST_SYNC: &str = "ffi.last_sync";
pub const CONFIG_KEY: &str = "ffi.config";
/// Same cadence as the server's periodic rescore.
pub const TRUST_MAX_AGE_SECS: i64 = 6 * 3600;

pub struct State {
    pub config: FfiConfig,
    pub api: Option<ApiClient>,
    /// Hex trust root parsed from `config.root_pubkey`.
    pub root: Option<String>,
}

pub struct Inner {
    pub store: Arc<SqliteStore>,
    state: RwLock<State>,
    /// Poster files fetched from the server (next to the database).
    pub poster_dir: std::path::PathBuf,
    /// Whether the configured server has `features::FILTERS` (D44): `FILTERS_UNKNOWN` until
    /// a `/health` answer, reset by a config change.
    server_filters: AtomicU8,
    #[cfg(feature = "downloads")]
    engine: tokio::sync::Mutex<Option<Arc<magnetar_core::downloads::Downloader>>>,
}

/// Lowercase 40-hex v1 infohash.
pub fn norm_infohash(s: &str) -> Result<String, FfiError> {
    let ih = s.trim().to_ascii_lowercase();
    if ih.len() == 40 && ih.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(ih)
    } else {
        Err(FfiError::Invalid {
            msg: format!("not a 40-hex infohash: {s:?}"),
        })
    }
}

const FILTERS_UNKNOWN: u8 = 0;
const FILTERS_YES: u8 = 1;
const FILTERS_NO: u8 = 2;

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

impl Inner {
    pub fn new(store: Arc<SqliteStore>, config: FfiConfig) -> Self {
        Inner {
            store,
            state: RwLock::new(State {
                config,
                api: None,
                root: None,
            }),
            poster_dir: "posters".into(),
            server_filters: AtomicU8::new(FILTERS_UNKNOWN),
            #[cfg(feature = "downloads")]
            engine: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_poster_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.poster_dir = dir;
        self
    }

    /// See `Magnetar::media_poster`.
    pub async fn media_poster(&self, media_id: i64, url: &str) -> Result<Option<String>, FfiError> {
        if !magnetar_core::api::is_api_path(url) {
            return Err(FfiError::Invalid {
                msg: format!("not a server poster URL: {url:?}"),
            });
        }
        let now = magnetar_core::now();
        let cached = self
            .store
            .poster(media_id, now)?
            .filter(|p| std::path::Path::new(&p.file).is_file());
        let fresh = cached
            .as_ref()
            .is_some_and(|p| now - p.fetched_at < crate::media::POSTER_REFRESH_SECS);
        // Past the TMDB keep limit a copy is only usable once the server revalidates it.
        let usable = |c: Option<crate::media::CachedPoster>| -> Result<Option<String>, FfiError> {
            match c {
                Some(p) if now - p.fetched_at >= crate::media::TMDB_MAX_AGE_SECS => {
                    let _ = std::fs::remove_file(&p.file);
                    self.store.forget_poster(media_id)?;
                    Ok(None)
                }
                c => Ok(c.map(|p| p.file)),
            }
        };
        let Some(api) = self.api().filter(|_| !fresh) else {
            return usable(cached);
        };
        let etag = cached.as_ref().and_then(|p| p.etag.clone());
        let fetched = match api.get_bytes(url, etag.as_deref()).await {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("poster {media_id}: {e}");
                return usable(cached);
            }
        };
        match fetched {
            Some(Fetched::NotModified) => {
                self.store.touch_poster(media_id, now)?;
                Ok(cached.map(|p| p.file))
            }
            Some(Fetched::Body {
                bytes,
                content_type,
                etag,
            }) => {
                let ext = match content_type.as_deref().unwrap_or("") {
                    t if t.starts_with("image/png") => "png",
                    t if t.starts_with("image/webp") => "webp",
                    t if t.starts_with("image/jpeg") => "jpg",
                    other => {
                        return Err(FfiError::Invalid {
                            msg: format!("poster content-type {other:?}"),
                        });
                    }
                };
                let io = |e: std::io::Error| FfiError::Other {
                    msg: format!("poster cache: {e}"),
                };
                std::fs::create_dir_all(&self.poster_dir).map_err(io)?;
                let file = self.poster_dir.join(format!("{media_id}.{ext}"));
                let tmp = self.poster_dir.join(format!("{media_id}.tmp"));
                std::fs::write(&tmp, &bytes).map_err(io)?;
                std::fs::rename(&tmp, &file).map_err(io)?;
                if let Some(old) = &cached
                    && std::path::Path::new(&old.file) != file
                {
                    let _ = std::fs::remove_file(&old.file);
                }
                let file = file.to_string_lossy().into_owned();
                self.store
                    .put_poster(media_id, &file, etag.as_deref(), bytes.len() as u64, now)?;
                for f in self.store.evict_posters(crate::media::POSTER_CACHE_BYTES)? {
                    let _ = std::fs::remove_file(f);
                }
                Ok(Some(file))
            }
            None => {
                // The server no longer has one.
                if let Some(old) = cached {
                    let _ = std::fs::remove_file(&old.file);
                    self.store.forget_poster(media_id)?;
                }
                Ok(None)
            }
        }
    }

    pub fn state(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn api(&self) -> Option<ApiClient> {
        self.state().api.clone()
    }

    pub fn require_api(&self) -> Result<ApiClient, FfiError> {
        self.api().ok_or_else(|| FfiError::Invalid {
            msg: "no server configured".into(),
        })
    }

    /// Validate, persist into settings (core keys + `ffi.config`), then swap in.
    pub fn apply_config(&self, config: FfiConfig) -> Result<(), FfiError> {
        let root = non_empty(&config.root_pubkey)
            .map(parse_pubkey)
            .transpose()?;
        let api = non_empty(&config.server_url)
            .map(|url| ApiClient::new(url, non_empty(&config.api_token).map(str::to_string)))
            .transpose()?;
        let seeds: Vec<String> = if config.seed_relays.is_empty() {
            DEFAULT_SEED_RELAYS.iter().map(|s| s.to_string()).collect()
        } else {
            config.seed_relays.clone()
        };
        let store = &self.store;
        store.set_setting_sync(keys::TRUST_ROOT, &json(&root))?;
        store.set_setting_sync(keys::TRUST_THRESHOLD, &config.trust_threshold.to_string())?;
        store.set_setting_sync(keys::SEED_RELAYS, &json(&seeds))?;
        store.set_setting_sync(CONFIG_KEY, &json(&StoredConfig::from(&config)))?;
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        *st = State { config, api, root };
        self.server_filters
            .store(FILTERS_UNKNOWN, Ordering::Relaxed);
        Ok(())
    }

    /// Remember what a `/health` answer of the configured server listed.
    pub fn note_features(&self, features: &[String]) {
        let has = features
            .iter()
            .any(|f| f == magnetar_core::api::features::FILTERS);
        let v = if has { FILTERS_YES } else { FILTERS_NO };
        self.server_filters.store(v, Ordering::Relaxed);
    }

    /// Whether `q` may go to the server: one older than D44 would ignore its release/media
    /// filters and answer unfiltered results (or reject `Sort::Rating`), so such a query uses
    /// the local cache, which applies them. Asks `/health` once when not known yet.
    pub async fn server_can_run(&self, api: &ApiClient, q: &SearchQuery) -> bool {
        if !q.needs_filter_feature() {
            return true;
        }
        if self.server_filters.load(Ordering::Relaxed) == FILTERS_UNKNOWN {
            match api.health().await {
                Ok(h) => self.note_features(&h.features),
                // Unreachable: the search itself fails and falls back to the cache.
                Err(_) => return true,
            }
        }
        self.server_filters.load(Ordering::Relaxed) != FILTERS_NO
    }

    /// Ffi query -> core query; `min_trust: None` means the configured threshold.
    pub fn query(&self, q: FfiSearchQuery) -> SearchQuery {
        let mut q: SearchQuery = q.into();
        if q.min_trust.is_none() {
            q.min_trust = Some(self.state().config.trust_threshold);
        }
        q.include_adult |= self.state().config.show_adult;
        q
    }

    pub fn crawl_config(&self, max_seconds: u32) -> CrawlConfig {
        let st = self.state();
        let mut cfg = CrawlConfig::phone_default();
        if max_seconds > 0 {
            cfg.budget = Duration::from_secs(max_seconds as u64);
        }
        if !st.config.seed_relays.is_empty() {
            cfg.seed_relays = st.config.seed_relays.clone();
        }
        cfg.root = st.root.clone();
        cfg
    }

    /// Apply phone-local pinned/hidden flags to server hits; drop hidden ones unless asked,
    /// and adult ones when `hide_adult` (D52: a server older than the adult filter ignores
    /// `include_adult`, so its `porn` hits are dropped here).
    pub fn overlay_flags(
        &self,
        page: &mut SearchPage,
        include_hidden: bool,
        hide_adult: bool,
    ) -> Result<(), FfiError> {
        if hide_adult {
            let before = page.hits.len();
            page.hits
                .retain(|h| h.category != magnetar_core::model::Category::Porn);
            page.total = page.total.saturating_sub((before - page.hits.len()) as u64);
        }
        let ids: Vec<String> = page.hits.iter().map(|h| h.infohash.clone()).collect();
        let flags = self.store.flags(&ids)?;
        for h in &mut page.hits {
            let (pinned, hidden) = flags.get(&h.infohash).copied().unwrap_or_default();
            h.pinned = pinned;
            h.hidden = hidden;
        }
        if !include_hidden {
            let before = page.hits.len();
            page.hits.retain(|h| !h.hidden);
            page.total = page.total.saturating_sub((before - page.hits.len()) as u64);
        }
        Ok(())
    }

    /// `core::notify::poll_saved_searches` semantics, but searching the server (local fallback
    /// per search): hits newer than `last_seen_created_at`; first run records the newest and
    /// returns nothing; watermark + `last_checked` persisted locally.
    pub async fn poll_via_server(
        &self,
        api: &ApiClient,
        now: i64,
    ) -> Result<Vec<(SavedSearch, Vec<TorrentSummary>)>, FfiError> {
        let threshold = self.state().config.trust_threshold;
        let mut out = Vec::new();
        for mut s in self.store.list_saved_searches_sync()? {
            if !s.notify {
                continue;
            }
            let mut q = s.query.clone();
            q.sort = Sort::Date;
            q.descending = true;
            q.offset = 0;
            q.min_trust.get_or_insert(threshold);
            q.include_adult |= self.state().config.show_adult;
            if let Some(w) = s.last_seen_created_at {
                q.since = Some(q.since.map_or(w, |since| since.max(w)));
            }
            let on_server = if self.server_can_run(api, &q).await {
                api.search(&q).await
            } else {
                Err(magnetar_core::Error::Invalid(
                    "the server is older than the release filters".into(),
                ))
            };
            let mut page = match on_server {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("saved search {} on server: {e}; using local", s.id);
                    self.store.search_sync(&q, now)?
                }
            };
            self.overlay_flags(&mut page, q.include_hidden, q.hides_adult())?;
            let newest = page.hits.iter().map(|h| h.created_at).max();
            let hits = match s.last_seen_created_at {
                None => {
                    s.last_seen_created_at = Some(newest.unwrap_or(now));
                    Vec::new()
                }
                Some(w) => {
                    s.last_seen_created_at = Some(newest.map_or(w, |n| n.max(w)));
                    page.hits.into_iter().filter(|h| h.created_at > w).collect()
                }
            };
            s.last_checked = Some(now);
            self.store.put_saved_search_sync(&s)?;
            out.push((s, hits));
        }
        Ok(out)
    }

    /// Start the phone download engine on first use. Must run on the Magnetar runtime: librqbit
    /// spawns its session tasks there.
    #[cfg(feature = "downloads")]
    pub async fn downloader(&self) -> Result<Arc<magnetar_core::downloads::Downloader>, FfiError> {
        let mut engine = self.engine.lock().await;
        if let Some(e) = engine.as_ref() {
            return Ok(e.clone());
        }
        let (out, state) = {
            let st = self.state();
            (st.config.download_dir.clone(), st.config.state_dir.clone())
        };
        if out.trim().is_empty() || state.trim().is_empty() {
            return Err(FfiError::Invalid {
                msg: "download_dir and state_dir must be configured".into(),
            });
        }
        for dir in [&out, &state] {
            std::fs::create_dir_all(dir).map_err(|e| FfiError::Invalid {
                msg: format!("download engine unavailable: {dir}: {e}"),
            })?;
        }
        let e = magnetar_core::downloads::Downloader::new(out.into(), state.into())
            .await
            .map_err(|e| FfiError::Invalid {
                msg: format!("download engine unavailable: {e}"),
            })?;
        let e = Arc::new(e);
        *engine = Some(e.clone());
        Ok(e)
    }

    #[cfg(not(feature = "downloads"))]
    pub async fn downloader(&self) -> Result<NoEngine, FfiError> {
        Err(FfiError::Invalid {
            msg: "built without the downloads feature".into(),
        })
    }

    pub async fn pause_all(&self) {
        #[cfg(feature = "downloads")]
        {
            let engine = self.engine.lock().await.clone();
            if let Some(e) = engine {
                e.pause_all().await;
            }
        }
    }
}

/// Stand-in engine type so the download methods type-check without the feature.
#[cfg(not(feature = "downloads"))]
pub struct NoEngine;

#[cfg(not(feature = "downloads"))]
impl NoEngine {
    pub async fn add_magnet(
        &self,
        _: &str,
    ) -> magnetar_core::Result<magnetar_core::model::DownloadInfo> {
        Err(magnetar_core::Error::Invalid("downloads disabled".into()))
    }
    pub fn list(&self) -> Vec<magnetar_core::model::DownloadInfo> {
        Vec::new()
    }
    pub async fn pause(&self, _: u64) -> magnetar_core::Result<()> {
        Ok(())
    }
    pub async fn resume(&self, _: u64) -> magnetar_core::Result<()> {
        Ok(())
    }
    pub async fn remove(&self, _: u64, _: bool) -> magnetar_core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magnetar_core::model::is_hex32;

    #[test]
    fn infohash_normalisation() {
        let ih = "08ADA5A7A6183AAE1E09D831DF6748D566095A10";
        assert_eq!(norm_infohash(ih).unwrap(), ih.to_ascii_lowercase());
        assert!(norm_infohash("xyz").is_err());
        assert!(is_hex32(&"a".repeat(64)));
    }
}
