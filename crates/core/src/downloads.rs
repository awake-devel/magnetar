//! librqbit wrapper shared by server and phone (ARCHITECTURE.md §8.3, android.md §4). Owner: server agent.
//!
//! Session JSON, fastresume and DHT state live under an explicit `state_dir` (never
//! `ProjectDirs`, which has no meaning on Android). A magnet whose metadata is not resolved
//! within [`ADD_WAIT`] is listed as a pending "initializing" download with a synthetic id until
//! librqbit hands back a real one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::dht::DhtPersistenceConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, DhtSessionConfig, Magnet, ManagedTorrent, Session,
    SessionOptions, SessionPersistenceConfig, TorrentStatsState,
};
use tokio::task::AbortHandle;

use crate::error::{Error, Result};
use crate::model::DownloadInfo;

/// How long `add_magnet` waits for metadata before returning a pending entry.
pub const ADD_WAIT: Duration = Duration::from_secs(15);
/// Synthetic ids for downloads still resolving metadata (librqbit ids are small counters).
const PENDING_BASE: u64 = 1 << 62;

struct Pending {
    name: String,
    infohash: String,
    error: Option<String>,
    task: AbortHandle,
}

type PendingMap = Arc<Mutex<HashMap<u64, Pending>>>;

pub struct Downloader {
    pub output_dir: PathBuf,
    session: Arc<Session>,
    pending: PendingMap,
    next_pending: AtomicU64,
}

fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("downloads: {e:#}"))
}

fn info(id: usize, h: &ManagedTorrent) -> DownloadInfo {
    let st = h.stats();
    let state = match st.state {
        TorrentStatsState::Initializing { .. } => "initializing",
        TorrentStatsState::Live if st.finished => "finished",
        TorrentStatsState::Live => "live",
        TorrentStatsState::Paused => "paused",
        TorrentStatsState::Error => "error",
    };
    let progress = if st.total_bytes > 0 {
        (st.progress_bytes as f64 / st.total_bytes as f64) as f32
    } else {
        0.0
    };
    let (down_bps, up_bps, peers) = st
        .live
        .as_ref()
        .map(|l| {
            (
                l.download_speed.as_bytes(),
                l.upload_speed.as_bytes(),
                l.snapshot.peer_stats.live,
            )
        })
        .unwrap_or_default();
    let infohash = h.info_hash().as_string();
    DownloadInfo {
        id: id as u64,
        name: h.name().unwrap_or_else(|| infohash.clone()),
        infohash,
        state: state.to_string(),
        progress,
        downloaded: st.progress_bytes,
        total: st.total_bytes,
        down_bps,
        up_bps,
        peers,
        error: st.error,
        output_dir: h.output_folder().display().to_string(),
    }
}

impl Downloader {
    /// Explicit session + DHT persistence under `state_dir` (never ProjectDirs).
    pub async fn new(output_dir: PathBuf, state_dir: PathBuf) -> Result<Self> {
        let session_dir = state_dir.join("session");
        for d in [&output_dir, &state_dir, &session_dir] {
            tokio::fs::create_dir_all(d)
                .await
                .map_err(|e| Error::Other(format!("create {}: {e}", d.display())))?;
        }
        let opts = SessionOptions {
            dht: Some(DhtSessionConfig {
                bootstrap_addrs: None,
                port: None,
                persistence: Some(DhtPersistenceConfig {
                    dump_interval: None,
                    config_filename: Some(state_dir.join("dht.json")),
                }),
            }),
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(session_dir),
            }),
            fastresume: true,
            client_name_and_version: Some(concat!("magnetar/", env!("CARGO_PKG_VERSION")).into()),
            ..Default::default()
        };
        let session = Session::new_with_opts(output_dir.clone(), opts)
            .await
            .map_err(other)?;
        Ok(Self {
            output_dir,
            session,
            pending: Arc::default(),
            next_pending: AtomicU64::new(PENDING_BASE),
        })
    }

    pub async fn add_magnet(&self, magnet: &str) -> Result<DownloadInfo> {
        let parsed =
            Magnet::parse(magnet).map_err(|e| Error::Invalid(format!("magnet: {e:#}")))?;
        let hash = parsed
            .as_id20()
            .ok_or_else(|| Error::Invalid("magnet has no BitTorrent v1 infohash".into()))?;
        if let Some(h) = self.session.get(TorrentIdOrHash::Hash(hash)) {
            return Ok(info(h.id(), &h));
        }
        let infohash = hash.as_string();
        if let Some(p) = self.pending_info_for(&infohash) {
            return Ok(p);
        }

        let session = self.session.clone();
        let url = magnet.to_string();
        let mut task = tokio::spawn(async move {
            let opts = AddTorrentOptions {
                overwrite: true,
                ..Default::default()
            };
            session
                .add_torrent(AddTorrent::from_url(url), Some(opts))
                .await
                .map(|r| r.into_handle())
        });
        match tokio::time::timeout(ADD_WAIT, &mut task).await {
            Ok(Ok(Ok(Some(h)))) => Ok(info(h.id(), &h)),
            Ok(Ok(Ok(None))) => Err(other("torrent was only listed")),
            Ok(Ok(Err(e))) => Err(Error::Invalid(format!("add torrent: {e:#}"))),
            Ok(Err(e)) => Err(other(e)),
            Err(_) => {
                // Still resolving metadata: track it and keep waiting in the background.
                let id = self.next_pending.fetch_add(1, Ordering::Relaxed);
                let name = parsed.name.clone().unwrap_or_else(|| infohash.clone());
                self.pending.lock().unwrap().insert(
                    id,
                    Pending {
                        name,
                        infohash,
                        error: None,
                        task: task.abort_handle(),
                    },
                );
                let pending = self.pending.clone();
                tokio::spawn(async move {
                    let res = task.await;
                    let mut map = pending.lock().unwrap();
                    match res {
                        Ok(Ok(_)) => {
                            map.remove(&id);
                        }
                        Ok(Err(e)) => {
                            if let Some(p) = map.get_mut(&id) {
                                p.error = Some(format!("{e:#}"));
                            }
                        }
                        // Aborted by remove(): the entry is already gone.
                        Err(_) => {}
                    }
                });
                Ok(self.pending_info(id).expect("just inserted"))
            }
        }
    }

    fn pending_entry(&self, id: u64, p: &Pending) -> DownloadInfo {
        DownloadInfo {
            id,
            name: p.name.clone(),
            infohash: p.infohash.clone(),
            state: if p.error.is_some() {
                "error"
            } else {
                "initializing"
            }
            .into(),
            progress: 0.0,
            downloaded: 0,
            total: 0,
            down_bps: 0,
            up_bps: 0,
            peers: 0,
            error: p.error.clone(),
            output_dir: self.output_dir.display().to_string(),
        }
    }

    fn pending_info(&self, id: u64) -> Option<DownloadInfo> {
        let map = self.pending.lock().unwrap();
        map.get(&id).map(|p| self.pending_entry(id, p))
    }

    fn pending_info_for(&self, infohash: &str) -> Option<DownloadInfo> {
        let map = self.pending.lock().unwrap();
        map.iter()
            .find(|(_, p)| p.infohash == infohash && p.error.is_none())
            .map(|(id, p)| self.pending_entry(*id, p))
    }

    pub fn list(&self) -> Vec<DownloadInfo> {
        let mut out: Vec<DownloadInfo> = self
            .session
            .with_torrents(|it| it.map(|(id, h)| info(id, h)).collect());
        {
            let map = self.pending.lock().unwrap();
            out.extend(map.iter().map(|(id, p)| self.pending_entry(*id, p)));
        }
        out.sort_by_key(|d| d.id);
        out
    }

    fn handle(&self, id: u64) -> Result<Arc<ManagedTorrent>> {
        if self.pending.lock().unwrap().contains_key(&id) {
            return Err(Error::Invalid(
                "download is still resolving its metadata".into(),
            ));
        }
        usize::try_from(id)
            .ok()
            .and_then(|id| self.session.get(TorrentIdOrHash::Id(id)))
            .ok_or_else(|| Error::Invalid(format!("no download with id {id}")))
    }

    pub async fn pause(&self, id: u64) -> Result<()> {
        let h = self.handle(id)?;
        self.session.pause(&h).await.map_err(other)
    }

    pub async fn resume(&self, id: u64) -> Result<()> {
        let h = self.handle(id)?;
        self.session.unpause(&h).await.map_err(other)
    }

    pub async fn remove(&self, id: u64, delete_files: bool) -> Result<()> {
        if let Some(p) = self.pending.lock().unwrap().remove(&id) {
            p.task.abort();
            return Ok(());
        }
        let h = self.handle(id)?;
        self.session
            .delete(TorrentIdOrHash::Id(h.id()), delete_files)
            .await
            .map_err(other)
    }

    pub async fn pause_all(&self) {
        let handles: Vec<Arc<ManagedTorrent>> = self
            .session
            .with_torrents(|it| it.map(|(_, h)| h.clone()).collect());
        for h in handles {
            if !h.is_paused()
                && let Err(e) = self.session.pause(&h).await
            {
                tracing::debug!("pause {}: {e:#}", h.id());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_lifecycle_offline() {
        let dir = std::env::temp_dir().join(format!("magnetar-dl-{}", rand::random::<u64>()));
        let d = Downloader::new(dir.join("out"), dir.join("state"))
            .await
            .unwrap();
        assert!(d.list().is_empty());
        assert!(matches!(
            d.add_magnet("not a magnet").await,
            Err(Error::Invalid(_))
        ));
        assert!(d.pause(7).await.is_err());
        assert!(d.remove(7, false).await.is_err());
        d.pause_all().await;
        assert!(dir.join("state/session").is_dir());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Real swarm: Sintel (android.md §4). Run with `--ignored`.
    #[tokio::test]
    #[ignore = "network"]
    async fn downloads_sintel_metadata() {
        let dir = std::env::temp_dir().join(format!("magnetar-dl-{}", rand::random::<u64>()));
        let d = Downloader::new(dir.join("out"), dir.join("state"))
            .await
            .unwrap();
        let magnet = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Sintel\
                      &tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce\
                      &tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce";
        let first = d.add_magnet(magnet).await.unwrap();
        assert_eq!(first.infohash, "08ada5a7a6183aae1e09d831df6748d566095a10");
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        let resolved = loop {
            let l = d.list();
            if let Some(x) = l.iter().find(|x| x.id < PENDING_BASE) {
                break x.clone();
            }
            assert!(std::time::Instant::now() < deadline, "metadata never resolved: {l:?}");
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        eprintln!("resolved: {resolved:?}");
        assert!(resolved.total > 100_000_000);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let now = d.list().into_iter().find(|x| x.id == resolved.id).unwrap();
        eprintln!("after 10 s: {now:?}");
        d.pause(resolved.id).await.unwrap();
        assert_eq!(
            d.list().into_iter().find(|x| x.id == resolved.id).unwrap().state,
            "paused"
        );
        d.resume(resolved.id).await.unwrap();
        d.remove(resolved.id, true).await.unwrap();
        assert!(d.list().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
