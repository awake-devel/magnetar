//! librqbit wrapper shared by server and phone (ARCHITECTURE.md §8.3). Owner: server agent.

use std::path::PathBuf;

use crate::model::DownloadInfo;

pub struct Downloader {
    pub output_dir: PathBuf,
    // session: Arc<librqbit::Session>
}

impl Downloader {
    /// Explicit session + DHT persistence under `state_dir` (never ProjectDirs).
    pub async fn new(output_dir: PathBuf, state_dir: PathBuf) -> crate::Result<Self> {
        let _ = (output_dir, state_dir);
        todo!("downloads::new")
    }
    pub async fn add_magnet(&self, magnet: &str) -> crate::Result<DownloadInfo> {
        let _ = magnet;
        todo!()
    }
    pub fn list(&self) -> Vec<DownloadInfo> {
        todo!()
    }
    pub async fn pause(&self, id: u64) -> crate::Result<()> {
        let _ = id;
        todo!()
    }
    pub async fn resume(&self, id: u64) -> crate::Result<()> {
        let _ = id;
        todo!()
    }
    pub async fn remove(&self, id: u64, delete_files: bool) -> crate::Result<()> {
        let _ = (id, delete_files);
        todo!()
    }
    pub async fn pause_all(&self) {
        todo!()
    }
}
