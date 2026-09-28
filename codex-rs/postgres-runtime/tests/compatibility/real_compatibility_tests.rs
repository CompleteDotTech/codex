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

// Run serially with bootstrap checks because these deliberately damage the same
// isolated namespace, then restore it before testing the next case.
pub async fn run(migrator: &PostgresPool, state: &Path) {
    let capabilities = ClientCapabilities {
        min_schema_format: 1,
        max_schema_format: 1,
        reader_version: 1,
        writer_version: 1,
    };
    let compatible = Ok(CompatibilityResult {
        schema_format: 1,
        activation_permitted: false,
    });
    for (damage, repair, error) in [
        (
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN format_version TYPE BIGINT",
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN format_version TYPE INTEGER",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN min_writer_version DROP NOT NULL; UPDATE codex_storage.codex_schema_meta SET min_writer_version = NULL",
            "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 1; ALTER TABLE codex_storage.codex_schema_meta ALTER COLUMN min_writer_version SET NOT NULL",
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
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_pkey",
            "ALTER TABLE codex_storage.codex_schema_meta ADD PRIMARY KEY (singleton)",
            CompatibilityError::MissingMetadata,
        ),
        (
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_min_reader_version_check; UPDATE codex_storage.codex_schema_meta SET min_reader_version = 0",
            "UPDATE codex_storage.codex_schema_meta SET min_reader_version = 1; ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_min_reader_version_check CHECK (min_reader_version > 0)",
            CompatibilityError::MissingMetadata,
        ),
        (
            "GRANT UPDATE ON codex_storage.codex_schema_meta TO codex_runtime",
            "REVOKE UPDATE ON codex_storage.codex_schema_meta FROM codex_runtime",
            CompatibilityError::Privilege,
        ),
        (
            "CREATE COLLATION codex_storage.unexpected_collation (provider = libc, locale = 'C')",
            "DROP COLLATION codex_storage.unexpected_collation",
            CompatibilityError::IncompatibleNamespace,
        ),
        (
            "DROP TABLE codex_storage.codex_schema_meta; CREATE VIEW codex_storage.codex_schema_meta AS SELECT TRUE AS singleton, 1 AS format_version, 1 AS min_reader_version, 1 AS min_writer_version",
            concat!(
                "DROP VIEW codex_storage.codex_schema_meta;",
                include_str!("../../migrations/0001_codex_storage_metadata.sql"),
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
    for (lock, query) in [
        (
            "LOCK TABLE codex_storage.codex_schema_meta IN ACCESS EXCLUSIVE MODE",
            "SELECT singleton,%",
        ),
        (
            "LOCK TABLE codex_storage._codex_pg_migrations IN ACCESS EXCLUSIVE MODE",
            "SELECT history.version,%",
        ),
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
                    let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock' AND query LIKE $2)")
                        .bind(backend).bind(query).fetch_one(&mut *observer).await.expect("observe blocked query");
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
    query_pool
        .close()
        .await
        .expect("close query cancellation pool");
}
