//! Postgres NOTICE spam is not sent (D46). Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

use magnetar_server::PgStore;

#[tokio::test]
async fn sessions_only_get_warnings_and_errors() {
    let Ok(url) = std::env::var("MAGNETAR_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED (no MAGNETAR_TEST_DATABASE_URL)");
        return;
    };
    let store = PgStore::connect(&url, 2).await.unwrap();
    let level: String = sqlx::query_scalar("SHOW client_min_messages")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(level, "warning");
    // The statement behind the production spam still works (the word is just not indexed).
    let long = "x".repeat(3000);
    let n: i32 = sqlx::query_scalar("SELECT length(to_tsvector('simple', $1)::text)")
        .bind(&long)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
}
