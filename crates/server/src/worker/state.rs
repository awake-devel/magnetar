//! What a worker keeps between restarts (D100): its DHT identity (external address, BEP 42
//! node ids, the ports they were made for) and up to 1000 good contacts, in one small JSON
//! file in the state dir (`MAGNETAR_WORKER_STATE_DIR`), written atomically every 10 minutes
//! and at stop. No seen-cache is kept: the server's triage is the fleet-wide dedupe, the
//! in-memory Bloom filter only saves requests. Also the heartbeat file Docker's health check
//! reads.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};

use magnetar_core::dht::{CompactNode, Identity};
use serde::{Deserialize, Serialize};

pub const DHT_FILE: &str = "dht.json";
/// Touched after every heartbeat the server accepted.
pub const HEARTBEAT_FILE: &str = "heartbeat";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct DhtState {
    external_ip: Option<String>,
    ids: Vec<String>,
    ports: Vec<u16>,
    nodes: Vec<String>,
}

/// Create the state dir (mode 700).
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// The saved identity (only when made for these ports) and contacts. A missing or unreadable
/// file is a fresh start.
pub fn load(dir: &Path, ports: &[u16]) -> (Option<Identity>, Vec<CompactNode>) {
    let Ok(raw) = std::fs::read_to_string(dir.join(DHT_FILE)) else {
        return (None, Vec::new());
    };
    let Ok(s) = serde_json::from_str::<DhtState>(&raw) else {
        tracing::warn!("worker: {DHT_FILE} unreadable, starting with a new DHT identity");
        return (None, Vec::new());
    };
    let identity = s
        .external_ip
        .as_deref()
        .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
        .filter(|_| s.ports == ports)
        .map(|external_ip| Identity {
            external_ip,
            ids: s
                .ids
                .iter()
                .filter_map(|h| hex::decode(h).ok()?.try_into().ok())
                .collect(),
        });
    let nodes = s
        .nodes
        .iter()
        .filter_map(|n| {
            let (id, addr) = n.split_once('@')?;
            Some(CompactNode {
                id: hex::decode(id).ok()?.try_into().ok()?,
                addr: addr.parse::<SocketAddrV4>().ok()?,
            })
        })
        .take(1000)
        .collect();
    (identity, nodes)
}

pub fn save(
    dir: &Path,
    identity: Option<&Identity>,
    ports: &[u16],
    nodes: &[CompactNode],
) -> std::io::Result<()> {
    let s = DhtState {
        external_ip: identity.map(|i| i.external_ip.to_string()),
        ids: identity
            .map(|i| i.ids.iter().map(hex::encode).collect())
            .unwrap_or_default(),
        ports: ports.to_vec(),
        nodes: nodes
            .iter()
            .take(1000)
            .map(|n| format!("{}@{}", hex::encode(n.id), n.addr))
            .collect(),
    };
    write_atomic(&dir.join(DHT_FILE), serde_json::to_string(&s)?.as_bytes())
}

pub fn touch_heartbeat(dir: &Path) {
    let _ = write_atomic(
        &dir.join(HEARTBEAT_FILE),
        magnetar_core::now().to_string().as_bytes(),
    );
}

fn write_atomic(path: &PathBuf, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_port_check() {
        let dir = std::env::temp_dir().join(format!("mg-worker-state-{}", rand::random::<u64>()));
        ensure_dir(&dir).unwrap();
        assert!(load(&dir, &[1]).0.is_none(), "fresh start");
        let id = Identity {
            external_ip: Ipv4Addr::new(86, 1, 2, 3),
            ids: vec![[7; 20], [8; 20]],
        };
        let nodes = vec![CompactNode {
            id: [1; 20],
            addr: "1.2.3.4:6881".parse().unwrap(),
        }];
        save(&dir, Some(&id), &[6891, 6892], &nodes).unwrap();
        let (got, n) = load(&dir, &[6891, 6892]);
        assert_eq!(got.unwrap().ids, id.ids);
        assert_eq!(n.len(), 1);
        assert!(load(&dir, &[6891]).0.is_none(), "other ports: new ids");
        std::fs::write(dir.join(DHT_FILE), "not json").unwrap();
        assert!(load(&dir, &[6891, 6892]).0.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
