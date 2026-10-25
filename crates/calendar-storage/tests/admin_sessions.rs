use calendar_application::SecretBytes;
use calendar_storage::Storage;
fn text(s: &SecretBytes) -> &str {
    std::str::from_utf8(s.expose()).unwrap()
}
#[tokio::test]
async fn session_lifecycle_is_identity_bound_atomic_and_durable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Storage::open(&path).await.unwrap();
    let grant = db
        .create_admin_session("alice", "policy", "synthetic-challenge", 100, 200)
        .await
        .unwrap();
    assert!(db
        .create_admin_session("alice", "policy", "synthetic-challenge", 100, 200)
        .await
        .is_err());
    for (id, policy, now) in [
        ("bob", "policy", 110),
        ("alice", "changed", 110),
        ("alice", "policy", 200),
        ("alice", "policy", 99),
    ] {
        assert!(db
            .check_admin_session(text(&grant.session), id, policy, now)
            .await
            .is_err());
    }
    assert!(db
        .consume_admin_write(
            text(&grant.session),
            "alice",
            "policy",
            "forged",
            text(&grant.nonce),
            110,
            false
        )
        .await
        .is_err());
    let (a, b) = tokio::join!(
        db.consume_admin_write(
            text(&grant.session),
            "alice",
            "policy",
            text(&grant.csrf),
            text(&grant.nonce),
            110,
            false
        ),
        db.consume_admin_write(
            text(&grant.session),
            "alice",
            "policy",
            text(&grant.csrf),
            text(&grant.nonce),
            110,
            false
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let next = a.or(b).unwrap();
    db.close().await;
    let db = Storage::open(&path).await.unwrap();
    assert!(db
        .consume_admin_write(
            text(&grant.session),
            "alice",
            "policy",
            text(&grant.csrf),
            text(&grant.nonce),
            110,
            false
        )
        .await
        .is_err());
    db.consume_admin_write(
        text(&grant.session),
        "alice",
        "policy",
        text(&grant.csrf),
        text(&next),
        110,
        true,
    )
    .await
    .unwrap();
    assert!(db
        .check_admin_session(text(&grant.session), "alice", "policy", 111)
        .await
        .is_err());
    assert!(db
        .check_admin_session(text(&grant.session), "alice", "policy", 101)
        .await
        .is_err());
    db.close().await;
}

#[tokio::test]
async fn failed_session_insert_does_not_burn_challenge_or_create_partial_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_admin BEFORE INSERT ON admin_sessions BEGIN SELECT RAISE(ABORT,'synthetic failure'); END").execute(db.pool()).await.unwrap();
    assert!(db
        .create_admin_session("alice", "policy", "challenge", 100, 200)
        .await
        .is_err());
    let uses: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_challenge_uses")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(uses, 0);
    sqlx::query("DROP TRIGGER fail_admin")
        .execute(db.pool())
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        db.create_admin_session("alice", "policy", "challenge", 100, 200),
        db.create_admin_session("alice", "policy", "challenge", 100, 200)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    db.close().await;
}
