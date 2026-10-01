use super::owner_query;
use super::settings;
use codex_postgres_runtime::ClientCapabilities;
use codex_postgres_runtime::CompatibilityError;
use codex_postgres_runtime::CompatibilityResult;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::RequiredAccess;
use codex_postgres_runtime::check_codex_storage_compatibility;
use pretty_assertions::assert_eq;
use sqlx::Acquire;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;

// Run serially with bootstrap checks because these deliberately damage the same
// isolated namespace, then restore it before testing the next case.
pub async fn run(migrator: &PostgresPool, state: &Path) {
    let capabilities = ClientCapabilities {
        min_schema_format: 17,
        max_schema_format: 17,
        reader_version: 17,
        writer_version: 17,
    };
    let compatible = Ok(CompatibilityResult {
        schema_format: 17,
        activation_permitted: false,
    });
    assert_eq!(
        check_codex_storage_compatibility(
            migrator,
            ClientCapabilities {
                reader_version: 0,
                ..capabilities
            },
            RequiredAccess::ReadOnly,
        )
        .await,
        Err(CompatibilityError::InvalidCapabilities),
    );
    assert_eq!(
        check_codex_storage_compatibility(
            migrator,
            ClientCapabilities {
                min_schema_format: 2,
                max_schema_format: 2,
                ..capabilities
            },
            RequiredAccess::ReadOnly,
        )
        .await,
        Err(CompatibilityError::UnsupportedSchema),
    );
    owner_query(
        migrator,
        "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 18",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(migrator, capabilities, RequiredAccess::ReadOnly).await,
        compatible,
    );
    assert_eq!(
        check_codex_storage_compatibility(migrator, capabilities, RequiredAccess::ReadWrite).await,
        Err(CompatibilityError::WriterTooOld),
    );
    owner_query(
        migrator,
        "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 17",
    )
    .await;
    for (damage, repair, error) in [
        (
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN format_version TYPE BIGINT",
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN format_version TYPE INTEGER",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN min_writer_version DROP NOT NULL; UPDATE codex_storage.codex_schema_meta SET min_writer_version = NULL",
            "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 17; ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN min_writer_version SET NOT NULL",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage._codex_pg_migrations ALTER COLUMN success TYPE TEXT USING success::text",
            "ALTER TABLE codex_storage._codex_pg_migrations ALTER COLUMN success TYPE BOOLEAN USING success::boolean",
            CompatibilityError::IncompatibleHistory,
        ),
        (
            "ALTER TABLE codex_storage._codex_pg_migrations ALTER COLUMN success DROP NOT NULL; UPDATE codex_storage._codex_pg_migrations SET success = NULL",
            "UPDATE codex_storage._codex_pg_migrations SET success = TRUE; ALTER TABLE codex_storage._codex_pg_migrations ALTER COLUMN success SET NOT NULL",
            CompatibilityError::IncompatibleHistory,
        ),
        (
            "ALTER TABLE codex_storage._codex_pg_migrations ADD CONSTRAINT unexpected_history_check CHECK (success)",
            "ALTER TABLE codex_storage._codex_pg_migrations DROP CONSTRAINT unexpected_history_check",
            CompatibilityError::IncompatibleHistory,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_pkey",
            "ALTER TABLE codex_storage.codex_schema_meta ADD PRIMARY KEY (singleton)",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_format_version_check",
            "ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_format_version_check CHECK (format_version > 0)",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN singleton DROP DEFAULT",
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN singleton SET DEFAULT TRUE",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_min_reader_version_check; UPDATE codex_storage.codex_schema_meta SET min_reader_version = 0",
            "UPDATE codex_storage.codex_schema_meta SET min_reader_version = 17; ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_min_reader_version_check CHECK (min_reader_version > 0)",
            CompatibilityError::MissingMetadata,
        ),
        (
            "GRANT UPDATE ON codex_storage.codex_schema_meta TO codex_runtime",
            "REVOKE UPDATE ON codex_storage.codex_schema_meta FROM codex_runtime",
            CompatibilityError::Privilege,
        ),
        (
            "GRANT UPDATE ON codex_storage.codex_schema_meta TO PUBLIC",
            "REVOKE UPDATE ON codex_storage.codex_schema_meta FROM PUBLIC",
            CompatibilityError::Privilege,
        ),
        (
            "CREATE COLLATION codex_storage.unexpected_collation (provider = libc, locale = 'C')",
            "DROP COLLATION codex_storage.unexpected_collation",
            CompatibilityError::IncompatibleNamespace,
        ),
        (
            "DROP TABLE codex_storage.codex_schema_meta; CREATE VIEW codex_storage.codex_schema_meta AS SELECT TRUE AS singleton, 17 AS format_version, 17 AS min_reader_version, 17 AS min_writer_version",
            concat!(
                "DROP VIEW codex_storage.codex_schema_meta;",
                include_str!("../../migrations/0001_codex_storage_metadata.sql"),
                "UPDATE codex_storage.codex_schema_meta SET format_version = 17, min_reader_version = 17, min_writer_version = 17;",
                "REVOKE ALL ON codex_storage.codex_schema_meta FROM codex_runtime; GRANT SELECT ON codex_storage.codex_schema_meta TO codex_runtime;"
            ),
            CompatibilityError::MissingMetadata,
        ),
        (
            "REVOKE SELECT ON codex_storage.codex_schema_meta FROM codex_owner",
            "GRANT SELECT ON codex_storage.codex_schema_meta TO codex_owner",
            CompatibilityError::Privilege,
        ),
        (
            "REVOKE SELECT ON codex_storage._codex_pg_migrations FROM codex_owner",
            "GRANT SELECT ON codex_storage._codex_pg_migrations TO codex_owner",
            CompatibilityError::Privilege,
        ),
    ] {
        owner_query(migrator, damage).await;
        assert_eq!(
            check_codex_storage_compatibility(migrator, capabilities, RequiredAccess::ReadOnly)
                .await,
            Err(error),
            "damage must be rejected: {damage}"
        );
        owner_query(migrator, repair).await;
        assert_eq!(
            check_codex_storage_compatibility(migrator, capabilities, RequiredAccess::ReadOnly)
                .await,
            compatible,
            "repair must restore compatibility: {repair}"
        );
    }

    owner_query(
        migrator,
        "INSERT INTO codex_storage._codex_pg_migrations (version, description, success, checksum, execution_time) SELECT extra, description, TRUE, checksum, execution_time FROM codex_storage._codex_pg_migrations CROSS JOIN generate_series(18, 20017) extra WHERE version = 1",
    )
    .await;
    let mut bounded_settings = settings(state, "migrator");
    bounded_settings.limits.max_connections = 1;
    let bounded_pool = PostgresPool::connect(bounded_settings)
        .await
        .expect("bounded history pool");
    assert_eq!(
        check_codex_storage_compatibility(&bounded_pool, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::IncompatibleHistory),
    );
    let mut bounded_connection = bounded_pool.acquire().await.expect("inspect bounded query");
    let bounded_history_query: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_prepared_statements WHERE statement LIKE '%FROM ONLY codex_storage._codex_pg_migrations history%' AND statement LIKE '%ORDER BY history.version LIMIT $3%')",
    )
    .fetch_one(&mut *bounded_connection)
    .await
    .expect("inspect server-side history limit");
    assert!(
        bounded_history_query,
        "history query must be bounded on the server"
    );
    drop(bounded_connection);
    bounded_pool
        .close()
        .await
        .expect("close bounded history pool");
    owner_query(
        migrator,
        "DELETE FROM codex_storage._codex_pg_migrations WHERE version > 17",
    )
    .await;

    let mut connection_settings = settings(state, "migrator");
    connection_settings.limits.max_connections = 1;
    let query_pool = PostgresPool::connect(connection_settings)
        .await
        .expect("query cancellation pool");
    let mut connection = query_pool.acquire().await.expect("inspect query backend");
    let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .expect("query backend PID");
    drop(connection);
    for lock in [
        "LOCK TABLE codex_storage.codex_schema_meta IN ACCESS EXCLUSIVE MODE",
        "LOCK TABLE codex_storage._codex_pg_migrations IN ACCESS EXCLUSIVE MODE",
    ] {
        let mut blocker = migrator.acquire().await.expect("lock connection");
        let mut transaction = blocker.begin().await.expect("begin relation lock");
        sqlx::query("SET LOCAL ROLE codex_owner")
            .execute(&mut *transaction)
            .await
            .expect("assume owner for lock");
        sqlx::query(lock)
            .execute(&mut *transaction)
            .await
            .expect("hold relation lock");
        let mut observer = migrator.acquire().await.expect("observe migrator session");
        let cancel = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock')")
                        .bind(backend).fetch_one(&mut *observer).await.expect("observe blocked query");
                    if waiting { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("compatibility must reach the locked data query");
            let cancelled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
                .bind(backend)
                .fetch_one(&mut *observer)
                .await
                .expect("cancel compatibility query");
            assert!(cancelled);
        };
        let (result, ()) = tokio::join!(
            check_codex_storage_compatibility(&query_pool, capabilities, RequiredAccess::ReadOnly),
            cancel,
        );
        assert_eq!(result, Err(CompatibilityError::Unavailable));
        transaction.rollback().await.expect("release relation lock");
        assert_eq!(
            check_codex_storage_compatibility(&query_pool, capabilities, RequiredAccess::ReadOnly)
                .await,
            compatible
        );
    }
    let mut timeout_connection = query_pool
        .acquire()
        .await
        .expect("timeout query connection");
    sqlx::raw_sql("SET lock_timeout = 0; SET statement_timeout = 0")
        .execute(&mut *timeout_connection)
        .await
        .expect("disable shorter fixture timeouts");
    drop(timeout_connection);
    let mut blocker = migrator.acquire().await.expect("timeout lock connection");
    let mut transaction = blocker.begin().await.expect("begin timeout lock");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner for timeout lock");
    sqlx::query("LOCK TABLE codex_storage.codex_schema_meta IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *transaction)
        .await
        .expect("hold timeout lock");
    let started = Instant::now();
    assert_eq!(
        check_codex_storage_compatibility(&query_pool, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::Timeout),
    );
    assert!(
        started.elapsed() < Duration::from_secs(29),
        "server timeout must precede the outer 30-second deadline"
    );
    transaction.rollback().await.expect("release timeout lock");
    assert_eq!(
        check_codex_storage_compatibility(&query_pool, capabilities, RequiredAccess::ReadOnly)
            .await,
        compatible,
    );
    query_pool
        .close()
        .await
        .expect("close query cancellation pool");
}
