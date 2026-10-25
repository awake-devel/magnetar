//! ntfy notifications + saved-search evaluation (D25). Owner: trust-sync agent.

use crate::model::{SavedSearch, TorrentSummary};
use crate::query::Sort;
use crate::store::Store;

/// ntfy message body limit (larger bodies become attachments).
pub const NTFY_MAX_MESSAGE: usize = 4096;
/// ntfy title limit.
pub const NTFY_MAX_TITLE: usize = 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NtfyConfig {
    pub base_url: String,
    pub topic: String,
    pub token: Option<String>,
}

/// Longest prefix of `s` within `max` bytes, on a char boundary.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// JSON publish to the root URL (ntfy docs, publish.md "Publish as JSON"). `click` e.g. a magnet or magnetar:// link.
pub async fn ntfy_publish(
    cfg: &NtfyConfig,
    title: &str,
    message: &str,
    click: Option<&str>,
) -> crate::Result<()> {
    if cfg.base_url.trim().is_empty() || cfg.topic.trim().is_empty() {
        return Err(crate::Error::Invalid(
            "ntfy url and topic are required".into(),
        ));
    }
    let mut body = serde_json::json!({
        "topic": cfg.topic.trim(),
        "title": truncate(title, NTFY_MAX_TITLE),
        "message": truncate(message, NTFY_MAX_MESSAGE),
        "tags": ["magnet"],
        "priority": 3,
    });
    if let Some(c) = click {
        body["click"] = c.into();
    }
    let http = crate::http::client(std::time::Duration::from_secs(15))?;
    let mut rb = http
        .post(format!("{}/", cfg.base_url.trim().trim_end_matches('/')))
        // Keep ntfy.sh from forwarding to Firebase (no Google services).
        .header("X-Firebase", "no")
        .json(&body);
    if let Some(t) = cfg.token.as_deref().filter(|t| !t.is_empty()) {
        rb = rb.bearer_auth(t);
    }
    let resp = rb.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let msg: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect();
        return Err(crate::Error::Server {
            status: status.as_u16(),
            msg,
        });
    }
    Ok(())
}

/// (title, message, click) for every saved search's new hits of one poll: a single notification
/// per poll however many searches matched, so the 15-min notifier sends at most 96 a day, within
/// ntfy.sh's 250 (D34). One matching search keeps the `format_hits` form; `None` if nothing is new.
pub fn format_digest(
    hits: &[(SavedSearch, Vec<TorrentSummary>)],
) -> Option<(String, String, Option<String>)> {
    let hits: Vec<_> = hits.iter().filter(|(_, h)| !h.is_empty()).collect();
    match hits.as_slice() {
        [] => None,
        [(s, h)] => Some(format_hits(s, h)),
        many => {
            let total: usize = many.iter().map(|(_, h)| h.len()).sum();
            let title = format!("{total} new results in {} saved searches", many.len());
            let mut message = String::new();
            for (s, h) in many {
                message.push_str(&format!("• {}: {}\n", s.name, h.len()));
            }
            Some((title, message.trim_end().to_string(), None))
        }
    }
}

/// (title, message, click) for one saved search's new hits.
pub fn format_hits(s: &SavedSearch, hits: &[TorrentSummary]) -> (String, String, Option<String>) {
    let title = match hits.len() {
        1 => format!("{}: 1 new result", s.name),
        n => format!("{}: {n} new results", s.name),
    };
    let mut message = String::new();
    for h in hits.iter().take(10) {
        message.push_str("• ");
        message.push_str(&h.title);
        message.push('\n');
    }
    if hits.len() > 10 {
        message.push_str(&format!("… and {} more\n", hits.len() - 10));
    }
    let click = match hits {
        [one] => Some(format!("magnetar://torrent/{}", one.infohash)),
        _ => None,
    };
    (title, message.trim_end().to_string(), click)
}

/// Run each saved search with notify=true; return hits newer than `last_seen_created_at`
/// (first run: record the newest and return nothing), and persist the new watermark.
pub async fn poll_saved_searches<S: Store + ?Sized>(
    store: &S,
    now: i64,
) -> crate::Result<Vec<(SavedSearch, Vec<TorrentSummary>)>> {
    let mut out = Vec::new();
    for mut s in store.list_saved_searches().await? {
        if !s.notify {
            continue;
        }
        let mut q = s.query.clone();
        q.sort = Sort::Date;
        q.descending = true;
        q.offset = 0;
        q.limit = crate::query::MAX_LIMIT;
        let first_run = s.last_seen_created_at.is_none();
        if let Some(wm) = s.last_seen_created_at {
            q.since = Some(q.since.map_or(wm + 1, |since| since.max(wm + 1)));
        }
        let hits: Vec<TorrentSummary> = store
            .search(&q, now)
            .await?
            .hits
            .into_iter()
            .filter(|h| s.last_seen_created_at.is_none_or(|wm| h.created_at > wm))
            .collect();
        let newest = hits.iter().map(|h| h.created_at).max();
        // First run with no match: watermark 0 so anything that later matches is new.
        s.last_seen_created_at = match (first_run, newest) {
            (true, None) => Some(0),
            (_, Some(n)) => Some(n.max(s.last_seen_created_at.unwrap_or(0))),
            (false, None) => s.last_seen_created_at,
        };
        s.last_checked = Some(now);
        store.put_saved_search(&s).await?;
        if !first_run && !hits.is_empty() {
            out.push((s, hits));
        }
    }
    Ok(out)
}
