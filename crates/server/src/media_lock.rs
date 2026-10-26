//! One writer at a time on `media` / `torrent_media` (D46).
//!
//! The IMDb enricher (D40), the anime enricher (D41) and the IMDb dataset swap each write many
//! `media` and `torrent_media` rows in one transaction, in whatever order their batch has, and
//! both enrichers adopt and update rows the other created. Two such transactions could lock
//! the same rows in opposite orders and deadlock ("deadlock detected" in production,
//! 2026-10-26). Each of them now takes this transaction-level advisory lock first, so they
//! queue instead (a batch holds it for about a second). A deadlock or serialization failure
//! that still happens (another writer, e.g. a manual link) is retried once after a random
//! pause.

use std::time::Duration;

use sqlx::PgConnection;

/// `pg_advisory_xact_lock` key ("mgmedia").
pub const MEDIA_LINKS_LOCK: i64 = 0x006d_676d_6564_6961;

/// Wait for the media-links lock; it is released when the transaction ends.
pub async fn lock(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MEDIA_LINKS_LOCK)
        .execute(conn)
        .await?;
    Ok(())
}

/// Deadlock (40P01) or serialization failure (40001): the transaction was rolled back and
/// can be run again.
pub fn is_conflict(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == "40P01" || c == "40001")
}

/// [`is_conflict`] anywhere in an `anyhow` chain.
pub fn is_conflict_any(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<sqlx::Error>().is_some_and(is_conflict))
}

/// 50–500 ms, so two writers that just collided do not collide again.
pub async fn retry_pause() {
    let ms = 50 + rand::random::<u64>() % 451;
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
