//! Installs the process destination budget (D150, `magnetar_core::netbudget`) from the config
//! and reports it.

use std::sync::Arc;
use std::time::Duration;

use magnetar_core::netbudget::{self, BudgetConfig, DestBudget};
use tokio::sync::watch;

use crate::config::Config;

/// How often waits and refusals are summarised in the log (only when there were some).
pub const REPORT_EVERY: Duration = Duration::from_secs(600);

/// Install the budget unless `MAGNETAR_NET_DESTS=0`; returns it when installed.
pub fn install(cfg: &Config) -> Option<Arc<DestBudget>> {
    if cfg.net_dests == 0 {
        tracing::warn!(
            "destination budget off (MAGNETAR_NET_DESTS=0): outbound connections other than \
             the DHT have no shared limit"
        );
        return None;
    }
    let b = Arc::new(DestBudget::new(BudgetConfig::new(
        cfg.net_dests as usize,
        f64::from(cfg.net_flows),
    )));
    if !netbudget::install(b.clone()) {
        return netbudget::global().cloned();
    }
    tracing::info!(
        "destination budget: {} public destinations per 300 s, {} new connections/s (D150)",
        cfg.net_dests,
        cfg.net_flows
    );
    if cfg.downloads {
        tracing::warn!(
            "MAGNETAR_DOWNLOADS=true: librqbit's DHT, trackers and peers are NOT under the \
             destination budget or any pace (CLAUDE.md network safety)"
        );
    }
    Some(b)
}

/// Log a summary every [`REPORT_EVERY`] when connections waited or were refused.
pub async fn report_loop(b: Arc<DestBudget>, mut stop: watch::Receiver<bool>) {
    let mut last = b.stats();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(REPORT_EVERY) => {}
            _ = crate::daemons::stop_requested(&mut stop) => return,
        }
        let now = b.stats();
        let (waited, refused) = (now.waited - last.waited, now.refused - last.refused);
        if waited > 0 || refused > 0 {
            let (dests, rate) = b.limits();
            tracing::warn!(
                "destination budget: {waited} connection(s) waited and {refused} were refused \
                 in {} min ({} destinations now, limits {dests} per 300 s and {rate:.1}/s, brake \
                 level {})",
                REPORT_EVERY.as_secs() / 60,
                now.dests_now,
                b.level()
            );
        }
        last = now;
    }
}
