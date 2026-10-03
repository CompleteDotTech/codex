use super::*;
/// Trusted owning-host guard. It retains the original operation/lock namespace;
/// this check is not a SQL commit receipt and must never mint a different run.
pub trait ReturnOperationFence: Send + Sync {
    fn check(&self, run_id: Uuid) -> Result<ActivationTarget, MigrationError>;
    fn check_current(&self) -> Result<(), MigrationError>;
}
#[derive(Clone, Copy)]
pub(super) enum FencePoint {
    Begin,
    Running,
    Retire,
    Abandon,
}
impl Migrator {
    pub(crate) fn check_return_owner(&self) -> Result<(), MigrationError> {
        match &self.return_fence {
            Some(fence) => fence.check_current(),
            None => Ok(()),
        }
    }
    pub fn with_return_fence(
        mut self,
        fence: Arc<dyn ReturnOperationFence>,
        namespace: Option<&codex_postgres_runtime::NamedNamespace>,
    ) -> Self {
        self.return_search_path = Some(codex_postgres_runtime::owned_namespace_search_path(
            namespace,
        ));
        self.return_fence = Some(fence);
        self
    }
    pub(super) async fn prepare_return_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), MigrationError> {
        let Some(path) = &self.return_search_path else {
            return Ok(());
        };
        // First statement after BEGIN: do not inherit role/database snapshot policy.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .execute(&mut **tx)
            .await
            .map_err(target)?;
        self.check_return_owner()?;
        // Explicit pg_temp last prevents temporary tables shadowing the selected namespace.
        sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
            .bind(path)
            .execute(&mut **tx)
            .await
            .map_err(target)?;
        Ok(())
    }
    pub(crate) fn check_return_fence(
        &self,
        run_id: Uuid,
    ) -> Result<Option<ActivationTarget>, MigrationError> {
        self.return_fence
            .as_ref()
            .map(|fence| fence.check(run_id))
            .transpose()
    }
    pub(super) async fn fence_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        run_id: Uuid,
        point: FencePoint,
    ) -> Result<(), MigrationError> {
        let Some(expected) = self.check_return_fence(run_id)? else {
            return Ok(());
        };
        let row = sqlx::query("SELECT state, run_id, dataset_id, generation FROM storage_activation WHERE singleton FOR UPDATE")
            .fetch_one(&mut **tx).await.map_err(target)?;
        let state: String = row.try_get("state").map_err(target)?;
        let held: Option<Uuid> = row.try_get("run_id").map_err(target)?;
        let dataset: Option<String> = row.try_get("dataset_id").map_err(target)?;
        let generation: i64 = row.try_get("generation").map_err(target)?;
        let ordinary = generation == expected.generation;
        let valid = match point {
            FencePoint::Begin | FencePoint::Abandon => {
                ordinary && (state == "open" || (state == "migrating" && held == Some(run_id)))
            }
            FencePoint::Running => ordinary && state == "migrating" && held == Some(run_id),
            FencePoint::Retire => {
                held == Some(run_id)
                    && ((ordinary && state == "migrating")
                        || (state == "retired"
                            && expected.generation.checked_add(1) == Some(generation)))
            }
        };
        if !valid || dataset.as_deref() != Some(expected.dataset_id.to_string().as_str()) {
            return Err(MigrationError::TargetBusy);
        }
        let run = sqlx::query("SELECT direction, source_fingerprint FROM storage_migration_runs WHERE run_id = $1 FOR UPDATE")
            .bind(run_id).fetch_optional(&mut **tx).await.map_err(target)?;
        match run {
            Some(run) => {
                let fingerprint = self.source.fingerprint().await.map_err(source)?;
                if run.try_get::<String, _>("direction").map_err(target)? != "export"
                    || run
                        .try_get::<String, _>("source_fingerprint")
                        .map_err(target)?
                        != fingerprint
                {
                    return Err(MigrationError::TargetBusy);
                }
            }
            None if matches!(point, FencePoint::Begin)
                || (matches!(point, FencePoint::Abandon) && state == "open") => {}
            None => return Err(MigrationError::TargetBusy),
        }
        // A row-lock wait must not preserve a pre-wait host namespace observation.
        self.check_return_fence(run_id)?;
        Ok(())
    }
    pub(super) async fn fenced_run_state(
        &self,
        run_id: Uuid,
        state: &str,
    ) -> Result<(), MigrationError> {
        let mut connection = self.connection().await?;
        let mut tx = connection.begin().await.map_err(target)?;
        self.prepare_return_transaction(&mut tx).await?;
        self.fence_transaction(&mut tx, run_id, FencePoint::Running)
            .await?;
        sqlx::query(
            "UPDATE storage_migration_runs SET state = $2, updated_at_ms = $3 WHERE run_id = $1",
        )
        .bind(run_id)
        .bind(state)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&mut *tx)
        .await
        .map_err(target)?;
        self.fence_transaction(&mut tx, run_id, FencePoint::Running)
            .await?;
        tx.commit().await.map_err(target)
    }
    pub(super) async fn fenced_digest(
        &self,
        run_id: Uuid,
        domain: Domain,
        digest: &DomainDigest,
    ) -> Result<(), MigrationError> {
        let mut connection = self.connection().await?;
        let mut tx = connection.begin().await.map_err(target)?;
        self.prepare_return_transaction(&mut tx).await?;
        self.fence_transaction(&mut tx, run_id, FencePoint::Running)
            .await?;
        sqlx::query("INSERT INTO storage_migration_domains (run_id, domain, done, row_count, digest) VALUES ($1, $2, TRUE, $3, $4) ON CONFLICT (run_id, domain) DO UPDATE SET done = TRUE, row_count = $3, digest = $4")
            .bind(run_id).bind(domain.name()).bind(i64::try_from(digest.count).unwrap_or(i64::MAX)).bind(&digest.digest).execute(&mut *tx).await.map_err(target)?;
        self.fence_transaction(&mut tx, run_id, FencePoint::Running)
            .await?;
        tx.commit().await.map_err(target)
    }
}
