use gm_openbot::{HostedControlPlane, HostedPlaneBackend, PostgresHostedStore, SecretBackend};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Duration;

fn pg_admin_url() -> Option<String> {
    match std::env::var("DT_TEST_DATABASE_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u.trim().to_string()),
        _ => {
            if std::env::var("DT_REQUIRE_POSTGRES_TESTS").as_deref() == Ok("1") {
                panic!("DT_REQUIRE_POSTGRES_TESTS=1 but DT_TEST_DATABASE_URL is not set");
            }
            eprintln!("skipping: DT_TEST_DATABASE_URL not set");
            None
        }
    }
}

fn db_url_with_name(admin_url: &str, db_name: &str) -> String {
    let (without_query, query) = match admin_url.split_once('?') {
        Some((base, q)) => (base, Some(q)),
        None => (admin_url, None),
    };
    let prefix = match without_query.rfind('/') {
        Some(idx) => &without_query[..idx],
        None => without_query,
    };
    match query {
        Some(q) => format!("{prefix}/{db_name}?{q}"),
        None => format!("{prefix}/{db_name}"),
    }
}

struct FreshPgDb {
    admin_url: String,
    db_name: String,
    db_url: String,
}

impl FreshPgDb {
    async fn create() -> Option<Self> {
        let admin_url = pg_admin_url()?;
        let db_name = format!("dt_test_{}", uuid::Uuid::new_v4().simple());
        let admin_pool = PgPool::connect(&admin_url)
            .await
            .expect("failed to connect to PostgreSQL admin URL");
        sqlx::query(&format!("CREATE DATABASE {db_name}"))
            .execute(&admin_pool)
            .await
            .expect("failed to create isolated test database");
        admin_pool.close().await;
        let db_url = db_url_with_name(&admin_url, &db_name);
        Some(Self {
            admin_url,
            db_name,
            db_url,
        })
    }

    async fn cleanup(self) {
        if let Ok(admin_pool) = PgPool::connect(&self.admin_url).await {
            let _ = sqlx::query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
            )
            .bind(&self.db_name)
            .execute(&admin_pool)
            .await;
            let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {}", self.db_name))
                .execute(&admin_pool)
                .await;
            admin_pool.close().await;
        }
    }
}

const TEST_MASTER_KEY: &[u8] = b"super-strong-postgres-master-key-32b";

#[tokio::test]
async fn pg_migrations_apply_and_are_idempotent() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = PostgresHostedStore::connect(&db.db_url, dir.path(), TEST_MASTER_KEY)
        .await
        .unwrap();
    store.run_migrations().await.unwrap();
    store.run_migrations().await.unwrap();
    store.ready_check().await.unwrap();

    let pool = PgPool::connect(&db.db_url).await.unwrap();
    let rows = sqlx::query("SELECT version FROM _dt_hosted_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap();
    let versions: Vec<i32> = rows.into_iter().map(|r| r.get::<i32, _>(0)).collect();
    assert_eq!(versions, vec![1, 2, 3, 4]);
    pool.close().await;
    drop(store);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_migrations_apply_and_are_idempotent");
}

#[tokio::test]
async fn pg_concurrent_migrations_are_serialized() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let store_a = PostgresHostedStore::connect(&db.db_url, dir_a.path(), TEST_MASTER_KEY)
        .await
        .unwrap();
    let store_b = PostgresHostedStore::connect(&db.db_url, dir_b.path(), TEST_MASTER_KEY)
        .await
        .unwrap();
    let (res_a, res_b) = tokio::join!(store_a.run_migrations(), store_b.run_migrations());
    assert!(res_a.is_ok(), "store_a migration failed: {res_a:?}");
    assert!(res_b.is_ok(), "store_b migration failed: {res_b:?}");

    let pool = PgPool::connect(&db.db_url).await.unwrap();
    let rows = sqlx::query("SELECT version FROM _dt_hosted_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap();
    let versions: Vec<i32> = rows.into_iter().map(|r| r.get::<i32, _>(0)).collect();
    assert_eq!(versions, vec![1, 2, 3, 4]);
    pool.close().await;
    drop(store_a);
    drop(store_b);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_concurrent_migrations_are_serialized");
}

#[tokio::test]
async fn pg_tenant_isolation() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        PostgresHostedStore::connect(&db.db_url, dir.path(), TEST_MASTER_KEY)
            .await
            .unwrap(),
    );
    let backend = HostedPlaneBackend::Postgres(Arc::clone(&store));

    backend
        .provision_workspace(
            "ws_alpha",
            "t_alpha",
            "alpha@example.com",
            "Alpha Workspace",
            "alice",
            "tok-alpha",
        )
        .await
        .unwrap();
    backend
        .provision_workspace(
            "ws_beta",
            "t_beta",
            "beta@example.com",
            "Beta Workspace",
            "bob",
            "tok-beta",
        )
        .await
        .unwrap();

    let alice_ws = backend.list_user_workspaces("alice").await.unwrap();
    assert_eq!(alice_ws.len(), 1);
    assert_eq!(alice_ws[0].ws_id, "ws_alpha");

    assert!(
        backend
            .authorize_membership("ws_alpha", "alice")
            .await
            .is_ok()
    );
    assert!(
        backend
            .authorize_membership("ws_beta", "alice")
            .await
            .is_err()
    );
    assert!(
        backend
            .authorize_workspace("ws_alpha", "tok-beta")
            .await
            .is_err()
    );

    backend
        .revoke_membership("ws_alpha", "alice")
        .await
        .unwrap();
    assert!(
        backend
            .authorize_membership("ws_alpha", "alice")
            .await
            .is_err()
    );
    assert!(
        backend
            .list_user_workspaces("alice")
            .await
            .unwrap()
            .is_empty()
    );

    drop(backend);
    drop(store);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_tenant_isolation");
}

#[tokio::test]
async fn pg_encrypted_secret_roundtrip_and_tamper() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = PostgresHostedStore::connect(&db.db_url, dir.path(), TEST_MASTER_KEY)
        .await
        .unwrap();
    store
        .provision_workspace(
            "ws_sec",
            "t_sec",
            "sec@example.com",
            "Secret Workspace",
            "alice",
        )
        .await
        .unwrap();

    let plaintext = "sk-live-very-secret-token-value";
    store
        .set_secret("ws_sec", "openai_api_key", plaintext)
        .await
        .unwrap();
    let roundtrip = store.get_secret("ws_sec", "openai_api_key").await.unwrap();
    assert_eq!(roundtrip, plaintext);

    let pool = PgPool::connect(&db.db_url).await.unwrap();
    let row = sqlx::query(
        "SELECT ciphertext FROM encrypted_secrets WHERE ws_id = 'ws_sec' AND account = 'openai_api_key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut cipher: Vec<u8> = row.get(0);
    assert_ne!(cipher, plaintext.as_bytes());
    assert!(!String::from_utf8_lossy(&cipher).contains(plaintext));

    cipher[0] ^= 0xFF;
    sqlx::query(
        "UPDATE encrypted_secrets SET ciphertext = $1 WHERE ws_id = 'ws_sec' AND account = 'openai_api_key'",
    )
    .bind(&cipher)
    .execute(&pool)
    .await
    .unwrap();

    let tampered = store.get_secret("ws_sec", "openai_api_key").await;
    assert!(
        tampered.is_err(),
        "tampered ciphertext must fail authentication"
    );

    pool.close().await;
    drop(store);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_encrypted_secret_roundtrip_and_tamper");
}

#[tokio::test]
async fn pg_sqlite_migration_with_verified_backup() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sqlite_path = dir.path().join("hosted-control.sqlite");
    let sqlite_workers = dir.path().join("sqlite-workers");
    let plane = HostedControlPlane::open(&sqlite_path, &sqlite_workers, TEST_MASTER_KEY).unwrap();
    plane
        .provision_workspace("ws_one", "t_one", "Workspace One", "tok-one")
        .unwrap();
    plane
        .provision_workspace("ws_two", "t_two", "Workspace Two", "tok-two")
        .unwrap();
    let enroll = plane
        .issue_enrollment("ws_one", "alice", "owner", 600)
        .unwrap();
    let _sess = plane
        .redeem_enrollment(&enroll.enrollment_token, 3600)
        .unwrap();
    plane
        .tenant_secret_backend("ws_one")
        .unwrap()
        .set_secret("provider_key", "sk-migrated-1")
        .unwrap();
    plane
        .tenant_secret_backend("ws_two")
        .unwrap()
        .set_secret("provider_key", "sk-migrated-2")
        .unwrap();
    drop(plane);

    let pg_workers = dir.path().join("pg-workers");
    let pg = PostgresHostedStore::connect(&db.db_url, &pg_workers, TEST_MASTER_KEY)
        .await
        .unwrap();
    let report = pg
        .migrate_from_sqlite_with_backup(&sqlite_path)
        .await
        .unwrap();
    let backup_bytes = std::fs::read(&report.backup_path).unwrap();
    let actual_sha = hex::encode(Sha256::digest(&backup_bytes));
    assert_eq!(report.backup_sha256, actual_sha);
    assert_eq!(report.workspaces_migrated, 2);
    assert_eq!(report.secrets_migrated, 2);
    assert_eq!(
        pg.get_secret("ws_one", "provider_key").await.unwrap(),
        "sk-migrated-1"
    );
    assert_eq!(
        pg.get_secret("ws_two", "provider_key").await.unwrap(),
        "sk-migrated-2"
    );

    // Wait >1s so VACUUM INTO timestamped backup filename does not collide, then verify Postgres idempotency.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = pg
        .migrate_from_sqlite_with_backup(&sqlite_path)
        .await
        .unwrap();
    assert_eq!(second.workspaces_migrated, 0);
    assert_eq!(second.secrets_migrated, 0);
    assert_eq!(pg.list_workspaces().await.unwrap().len(), 2);

    drop(pg);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_sqlite_migration_with_verified_backup");
}

#[tokio::test]
async fn pg_sessions_and_enrollment() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        PostgresHostedStore::connect(&db.db_url, dir.path(), TEST_MASTER_KEY)
            .await
            .unwrap(),
    );
    let backend = HostedPlaneBackend::Postgres(Arc::clone(&store));
    backend
        .provision_workspace(
            "ws_sess",
            "t_sess",
            "sess@example.com",
            "Session Workspace",
            "alice",
            "tok-sess",
        )
        .await
        .unwrap();

    let enrolled = backend
        .issue_enrollment("ws_sess", "alice", "owner", 600)
        .await
        .unwrap();
    let session = backend
        .redeem_enrollment(&enrolled.enrollment_token, 3600)
        .await
        .unwrap();
    assert_eq!(session.ws_id, "ws_sess");
    assert_eq!(session.user_id, "alice");

    // Single-use enrollment token cannot be redeemed twice
    assert!(
        backend
            .redeem_enrollment(&enrolled.enrollment_token, 3600)
            .await
            .is_err()
    );

    let authorized = backend
        .authorize_session(&session.session_id, Some("ws_sess"))
        .await
        .unwrap();
    assert_eq!(authorized.session_id, session.session_id);

    backend.revoke_session(&session.session_id).await.unwrap();
    assert!(
        backend
            .authorize_session(&session.session_id, Some("ws_sess"))
            .await
            .is_err()
    );

    drop(backend);
    drop(store);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_sessions_and_enrollment");
}

#[tokio::test]
async fn pg_backend_kind_reports_postgres() {
    let Some(db) = FreshPgDb::create().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        PostgresHostedStore::connect(&db.db_url, dir.path(), TEST_MASTER_KEY)
            .await
            .unwrap(),
    );
    let backend = HostedPlaneBackend::Postgres(store);
    assert_eq!(backend.backend_kind(), "postgres");
    drop(backend);
    db.cleanup().await;
    println!("POSTGRES_TEST_RAN pg_backend_kind_reports_postgres");
}
