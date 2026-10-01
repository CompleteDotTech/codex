//! Handing authority back from PostgreSQL to this home's local files.
//!
//! The dataset is exported into a staging directory inside the home while it is closed to every
//! writer, verified, and waits in `Ready`. Activating swaps the staged files in, keeps what they
//! replace in a verified backup, and retires the dataset. Nothing is deleted.

use crate::BlockerCode;
use crate::PlanAction;
use crate::StorageError;
use crate::journal::OperationRecord;
use crate::journal::OperationState;
use crate::ops::Confirmation;
use crate::ops::RecoveryKind;
use crate::ops::RecoveryReport;
use crate::service::StorageService;
use codex_state::SqliteConfig;
use codex_storage_authority::read_cutover;
use codex_storage_migration::Migrator;
use codex_storage_migration::RecoveryOutcome;
use codex_storage_migration::ReturnCutover;
use codex_storage_migration::SqliteSource;
use codex_storage_migration::SqliteTarget;
use codex_storage_migration::read_plan;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::PathBuf;
use uuid::Uuid;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn internal<E>(_: E) -> StorageError {
    StorageError(BlockerCode::Internal)
}

impl StorageService {
    pub(crate) fn staged_home(&self, operation_id: Uuid) -> PathBuf {
        self.inputs
            .codex_home
            .join("storage-staging")
            .join(operation_id.to_string())
    }

    fn staged_config(&self, staged_home: &std::path::Path) -> Result<SqliteConfig, StorageError> {
        AbsolutePathBuf::from_absolute_path_checked(staged_home)
            .map(SqliteConfig::from_sqlite_home)
            .map_err(internal)
    }

    fn staged_source(&self, config: SqliteConfig) -> SqliteSource {
        SqliteSource::new(config).relocated_from(self.inputs.codex_home.clone())
    }

    /// Export the authoritative PostgreSQL dataset into a staged home and verify it. The
    /// operation then waits in `Ready`; nothing local changes until the return is activated.
    pub async fn start_return(
        &self,
        operation_id: Uuid,
        plan_id: Uuid,
        confirmation: Confirmation,
    ) -> Result<OperationRecord, StorageError> {
        if let Some(existing) = self.journal.read(operation_id).map_err(internal)? {
            return Ok(existing);
        }
        if !confirmation.writers_stopped {
            return Err(StorageError(BlockerCode::NotConfirmed));
        }
        let plan = self.plan(PlanAction::Return).await?;
        if plan.plan_id != plan_id {
            return Err(StorageError(BlockerCode::StalePlan));
        }
        if let Some(blocker) = plan.blockers.first() {
            return Err(StorageError(*blocker));
        }
        let mut record = OperationRecord {
            operation_id,
            action: PlanAction::Return,
            plan_digest: plan.digest,
            state: OperationState::Planned,
            run_id: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            blocker: None,
            copied: Vec::new(),
        };
        self.journal.create(&record).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                StorageError(BlockerCode::OperationConflict)
            } else {
                internal(error)
            }
        })?;
        self.export_and_verify(&mut record).await?;
        Ok(record)
    }

    async fn export_and_verify(&self, record: &mut OperationRecord) -> Result<(), StorageError> {
        let storage = match self.connect().await {
            Ok(storage) => storage,
            Err(error) => return Err(self.fail(record, error.0)),
        };
        self.save(record, OperationState::Copying)?;
        let staged_home = self.staged_home(record.operation_id);
        std::fs::create_dir_all(&staged_home).map_err(internal)?;
        let config = self.staged_config(&staged_home)?;
        let target = match SqliteTarget::create(
            config.clone(),
            self.inputs.codex_home.clone(),
            &self.inputs.default_model_provider_id,
        )
        .await
        {
            Ok(target) => target,
            Err(_) => {
                storage.close().await;
                return Err(self.fail(record, BlockerCode::StagingFailed));
            }
        };
        let exporter = Migrator::new(self.staged_source(config), storage.pool().clone());
        let summary = match exporter.export(&target).await {
            Ok(summary) => summary,
            Err(error) => {
                target.close().await;
                storage.close().await;
                return Err(self.fail(record, error.into()));
            }
        };
        record.run_id = Some(summary.run_id);
        record.copied = summary
            .domains
            .iter()
            .map(|(domain, rows)| (domain.name().to_string(), *rows))
            .collect();
        self.save(record, OperationState::Verifying)?;
        let verified = exporter.verify(summary.run_id).await;
        target.close().await;
        storage.close().await;
        if let Err(error) = verified {
            return Err(self.fail(record, error.into()));
        }
        self.save(record, OperationState::Ready)
    }

    pub(crate) async fn activate_return(
        &self,
        mut record: OperationRecord,
    ) -> Result<OperationRecord, StorageError> {
        let run_id = record
            .run_id
            .ok_or(StorageError(BlockerCode::OperationConflict))?;
        let storage = self.connect().await?;
        self.save(&mut record, OperationState::Committing)?;
        let staged_home = self.staged_home(record.operation_id);
        let config = self.staged_config(&staged_home)?;
        let returning = ReturnCutover::new(
            self.inputs.codex_home.clone(),
            staged_home,
            Migrator::new(self.staged_source(config), storage.pool().clone()),
        );
        // An interrupted earlier attempt left an intent behind; settle it before starting.
        let result = match returning.recover().await {
            Ok(RecoveryOutcome::RolledForward { .. }) => Ok(()),
            Ok(_) => returning.execute(run_id).await.map(|_| ()),
            Err(error) => Err(error),
        };
        storage.close().await;
        match result {
            Ok(()) => {
                self.save(&mut record, OperationState::Active)?;
                Ok(record)
            }
            Err(codex_storage_migration::CutoverError::Uncertain) => {
                record.blocker = Some(BlockerCode::CutoverInProgress);
                self.save(&mut record, OperationState::Committing)?;
                Err(StorageError(BlockerCode::CutoverInProgress))
            }
            Err(error) => Err(self.fail(&mut record, error.into())),
        }
    }

    /// Settle an interrupted return from the evidence on both sides.
    pub(crate) async fn recover_return(&self) -> Result<RecoveryReport, StorageError> {
        let plan = read_plan(&self.inputs.codex_home).map_err(internal)?;
        let Some(plan) = plan else {
            return Ok(RecoveryReport {
                outcome: RecoveryKind::Idle,
                operations: Vec::new(),
            });
        };
        let storage = self.connect().await?;
        let config = self.staged_config(&plan.staged_home)?;
        let returning = ReturnCutover::new(
            self.inputs.codex_home.clone(),
            plan.staged_home,
            Migrator::new(self.staged_source(config), storage.pool().clone()),
        );
        let outcome = returning.recover().await;
        storage.close().await;
        let outcome = outcome.map_err(|error| StorageError(error.into()))?;
        let mut touched = Vec::new();
        for mut record in self.journal.list().into_iter().filter(|record| {
            record.action == PlanAction::Return && record.state == OperationState::Committing
        }) {
            match outcome {
                RecoveryOutcome::RolledForward { .. } => {
                    record.blocker = None;
                    self.save(&mut record, OperationState::Active)?;
                }
                // The dataset was reopened, so a new return needs a new export.
                RecoveryOutcome::RolledBack => {
                    record.blocker = None;
                    self.save(&mut record, OperationState::Cancelled)?;
                }
                RecoveryOutcome::Idle => continue,
            }
            touched.push(record);
        }
        Ok(RecoveryReport {
            outcome: match outcome {
                RecoveryOutcome::Idle => RecoveryKind::Idle,
                RecoveryOutcome::RolledForward { .. } => RecoveryKind::RolledForward,
                RecoveryOutcome::RolledBack => RecoveryKind::RolledBack,
            },
            operations: touched,
        })
    }

    /// Cancel a return that has not retired the dataset. The dataset is reopened unchanged.
    pub(crate) async fn cancel_return(
        &self,
        mut record: OperationRecord,
    ) -> Result<OperationRecord, StorageError> {
        let storage = self.connect().await?;
        let staged_home = self.staged_home(record.operation_id);
        let config = self.staged_config(&staged_home)?;
        let migrator = Migrator::new(self.staged_source(config), storage.pool().clone());
        let returning = ReturnCutover::new(
            self.inputs.codex_home.clone(),
            staged_home,
            Migrator::new(
                self.staged_source(self.staged_config(&self.staged_home(record.operation_id))?),
                storage.pool().clone(),
            ),
        );
        if read_cutover(&self.inputs.codex_home)
            .map_err(internal)?
            .is_some()
        {
            returning
                .abort()
                .await
                .map_err(|error| StorageError(error.into()))?;
        }
        if let Some(run_id) = record.run_id {
            // A run that was never retired only read the dataset, so abandoning it reopens it.
            let state = migrator
                .activation_state()
                .await
                .map_err(|error| StorageError(error.into()))?;
            if state.migrating && state.run_id == Some(run_id) {
                migrator
                    .abandon(run_id)
                    .await
                    .map_err(|error| StorageError(error.into()))?;
            }
        }
        storage.close().await;
        record.blocker = None;
        self.save(&mut record, OperationState::Cancelled)?;
        Ok(record)
    }
}
