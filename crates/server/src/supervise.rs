//! Named background daemons (D147). `serve` starts about twenty loops; before D147 their
//! handles were only awaited at shutdown, so a loop that returned early or panicked vanished
//! without a word (a dead router health loop meant no brake at all). Every daemon is now
//! spawned here under a name: an exit or a panic before shutdown is logged at ERROR with that
//! name and recorded, and `/health` then answers `status: "degraded"`. Nothing is restarted:
//! a loop that dies has a bug, and restarting it would hide that.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Which daemons stopped before shutdown (shared with the API's `/health`).
#[derive(Clone, Debug, Default)]
pub struct DaemonHealth(Arc<Mutex<Vec<&'static str>>>);

impl DaemonHealth {
    /// Names of the daemons that exited or panicked while the server was running.
    pub fn dead(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }

    fn record(&self, name: &'static str) {
        self.0.lock().unwrap().push(name);
    }
}

pub struct Daemons {
    tasks: Vec<(&'static str, JoinHandle<()>)>,
    health: DaemonHealth,
    stop: watch::Receiver<bool>,
}

impl Daemons {
    pub fn new(stop: watch::Receiver<bool>) -> Self {
        Self {
            tasks: Vec::new(),
            health: DaemonHealth::default(),
            stop,
        }
    }

    pub fn health(&self) -> DaemonHealth {
        self.health.clone()
    }

    /// Spawn `fut` as the daemon `name`.
    pub fn spawn<F>(&mut self, name: &'static str, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.adopt(name, tokio::spawn(fut));
    }

    /// Watch a task that is already running (e.g. a pool of workers spawned elsewhere).
    pub fn adopt(&mut self, name: &'static str, task: JoinHandle<()>) {
        let (health, stop) = (self.health.clone(), self.stop.clone());
        let watcher = tokio::spawn(async move {
            match task.await {
                Ok(()) if *stop.borrow() => {}
                Ok(()) => {
                    tracing::error!("daemon {name} exited before shutdown; it is not restarted");
                    health.record(name);
                }
                Err(e) if e.is_panic() => {
                    let p = e.into_panic();
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic".into());
                    tracing::error!("daemon {name} panicked: {msg}; it is not restarted");
                    health.record(name);
                }
                Err(_) => {} // cancelled: only at runtime shutdown
            }
        });
        self.tasks.push((name, watcher));
    }

    /// After shutdown was requested: wait for each daemon, at most `grace` each.
    pub async fn join(self, grace: Duration) {
        for (name, t) in self.tasks {
            if tokio::time::timeout(grace, t).await.is_err() {
                tracing::warn!("daemon {name} did not stop in {} s", grace.as_secs());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn early_exit_and_panic_are_recorded_by_name() {
        let (tx, rx) = watch::channel(false);
        let mut d = Daemons::new(rx.clone());
        let health = d.health();
        d.spawn("quits", async {});
        d.spawn("panics", async { panic!("boom") });
        let mut wait = rx.clone();
        d.spawn("waits", async move {
            let _ = wait.wait_for(|v| *v).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut dead = health.dead();
        dead.sort();
        assert_eq!(dead, ["panics", "quits"]);
        tx.send(true).unwrap();
        d.join(Duration::from_secs(1)).await;
        // A daemon that stops because shutdown was asked is not "dead".
        assert_eq!(health.dead().len(), 2);
    }

    #[tokio::test]
    async fn adopted_tasks_are_watched_too() {
        let (_tx, rx) = watch::channel(false);
        let mut d = Daemons::new(rx);
        let health = d.health();
        d.adopt("pool", tokio::spawn(async {}));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(health.dead(), ["pool"]);
    }
}
