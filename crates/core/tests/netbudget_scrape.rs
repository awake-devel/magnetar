//! D150: a scrape the destination budget refused is not the tracker's fault, so the tracker
//! gets no backoff. A separate test binary, because the budget is process-wide.
//! No network traffic: the only tracker is an IP literal the full budget refuses before
//! anything is sent, and the DHT is off.

use std::sync::Arc;
use std::time::Duration;

use magnetar_core::liveness::{LivenessConfig, Scraper};
use magnetar_core::model::LivenessSource;
use magnetar_core::netbudget::{self, BudgetConfig, DestBudget};

#[tokio::test]
async fn a_scrape_the_budget_refused_does_not_back_the_tracker_off() {
    // Room for one destination, and it is taken.
    let mut cfg = BudgetConfig::new(1, 1000.0);
    cfg.max_wait = Duration::from_millis(100);
    let b = Arc::new(DestBudget::new(cfg));
    assert!(netbudget::install(b.clone()));
    netbudget::admit("test", "8.8.8.8".parse().unwrap())
        .await
        .unwrap();

    let scraper = Scraper::new(LivenessConfig {
        use_dht: false,
        tracker_timeout: Duration::from_millis(400),
        ..Default::default()
    });
    let tracker = "udp://9.9.9.9:6969/announce";
    let items = vec![("aa".repeat(20), vec![tracker.to_string()])];
    for pass in 0..2 {
        let (out, report) = scraper.check(&items, 1000).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, LivenessSource::None, "pass {pass}");
        // The tracker was tried again on the second pass (not skipped as backing off)…
        assert_eq!(report.trackers_backed_off, 0, "pass {pass}");
        // …and holds no failure and no backoff.
        assert_eq!(
            scraper.tracker_state(tracker),
            Some((0, false)),
            "pass {pass}"
        );
    }
    let s = b.stats();
    assert_eq!(s.refused, 2, "both passes were refused by the budget");
    assert_eq!(s.dests_now, 1, "the refused destination was never admitted");
}
