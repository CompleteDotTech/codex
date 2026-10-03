//! Handing authority back from PostgreSQL to local files.
//!
//! The dataset is exported into a staged home while it is closed to writers, verified, and then
//! swapped in: the intent is recorded, PostgreSQL is retired, the staged files replace the live
//! ones with everything they replace kept in a backup, and the authority records flip last.
//! [`ReturnCutover::recover`] settles an interruption from the evidence on both sides. Nothing
//! is deleted, and the retired dataset stays in PostgreSQL.

use crate::ActivationTarget;
use crate::CutoverError;
use crate::MigrationError;
use crate::Migrator;
use crate::RecoveryOutcome;
use crate::install;
use codex_storage_authority::ActiveBackend;
use codex_storage_authority::CutoverIntent;
use codex_storage_authority::LocalAuthority;
use codex_storage_authority::abandon_cutover;
use codex_storage_authority::begin_cutover;
use codex_storage_authority::complete_cutover;
use codex_storage_authority::read_cutover;
use std::path::PathBuf;
use uuid::Uuid;

fn io_error(error: std::io::Error) -> CutoverError {
    CutoverError::Authority(error.to_string())
}

fn authority(error: codex_storage_authority::AuthorityError) -> CutoverError {
    CutoverError::Authority(error.to_string())
}

/// The return of one home from one retired dataset.
pub struct ReturnCutover {
    home: PathBuf,
    staged_home: PathBuf,
    migrator: Migrator,
}

impl ReturnCutover {
    /// `migrator` must be the export migrator whose source is the staged home.
    pub fn new(home: PathBuf, staged_home: PathBuf, migrator: Migrator) -> Self {
        Self {
            home,
            staged_home,
            migrator,
        }
    }

    /// Record the intent and the swap plan. Nothing else changes yet.
    pub fn prepare(&self, run_id: Uuid) -> Result<CutoverIntent, CutoverError> {
        let intent = begin_cutover(&self.home, run_id, ActiveBackend::Local).map_err(authority)?;
        if let Err(error) = install::plan_install(&self.home, &self.staged_home, run_id) {
            abandon_cutover(&self.home, &intent).map_err(authority)?;
            return Err(io_error(error));
        }
        Ok(intent)
    }

    /// Close the dataset to every writer for good. Safe to repeat.
    pub async fn publish(&self, intent: &CutoverIntent) -> Result<(), CutoverError> {
        let generation = i64::try_from(intent.to_generation).map_err(|_| CutoverError::Conflict)?;
        self.migrator
            .retire(
                intent.run_id,
                ActivationTarget {
                    dataset_id: intent.dataset_id,
                    generation,
                },
            )
            .await?;
        Ok(())
    }

    /// Replace the live files with the staged ones, keeping what they replace. Safe to repeat.
    pub fn install(&self) -> Result<(), CutoverError> {
        let plan = install::read_plan(&self.home)
            .map_err(io_error)?
            .ok_or_else(|| CutoverError::Authority("the install plan is missing".to_string()))?;
        install::install(&self.home, &plan).map_err(io_error)
    }

    /// Flip the authority records to local, then forget the plan. Safe to repeat.
    pub fn finish(&self, intent: &CutoverIntent) -> Result<LocalAuthority, CutoverError> {
        let moved = complete_cutover(&self.home, intent).map_err(authority)?;
        install::discard_plan(&self.home).map_err(io_error)?;
        Ok(moved)
    }

    /// Run the whole protocol for a verified export.
    pub async fn execute(&self, run_id: Uuid) -> Result<LocalAuthority, CutoverError> {
        let intent = self.prepare(run_id)?;
        match self.publish(&intent).await {
            Ok(()) => {}
            Err(CutoverError::Destination(
                refusal @ (MigrationError::NotVerified
                | MigrationError::TargetBusy
                | MigrationError::GenerationNotAdvancing),
            )) => {
                abandon_cutover(&self.home, &intent).map_err(authority)?;
                install::discard_plan(&self.home).map_err(io_error)?;
                return Err(CutoverError::Destination(refusal));
            }
            Err(_) => return Err(CutoverError::Uncertain),
        }
        self.install()?;
        self.finish(&intent)
    }

    /// Settle an interrupted return from the evidence on both sides.
    pub async fn recover(&self) -> Result<RecoveryOutcome, CutoverError> {
        let Some(intent) = read_cutover(&self.home).map_err(authority)? else {
            // A plan without an intent belongs to a return that already completed.
            install::discard_plan(&self.home).map_err(io_error)?;
            return Ok(RecoveryOutcome::Idle);
        };
        if intent.target != ActiveBackend::Local {
            return Err(CutoverError::Conflict);
        }
        let remote = self.migrator.activation_state().await?;
        let to_generation =
            i64::try_from(intent.to_generation).map_err(|_| CutoverError::Conflict)?;
        if remote.retired
            && remote.run_id == Some(intent.run_id)
            && remote.generation == to_generation
            && remote.dataset_id == Some(intent.dataset_id)
        {
            self.install()?;
            let moved = self.finish(&intent)?;
            return Ok(RecoveryOutcome::RolledForward {
                generation: moved.identity.generation,
            });
        }
        let from_generation =
            i64::try_from(intent.from_generation).map_err(|_| CutoverError::Conflict)?;
        if remote.retired
            || remote.generation != from_generation
            || remote.run_id != Some(intent.run_id)
            || remote.dataset_id != Some(intent.dataset_id)
        {
            return Err(CutoverError::Conflict);
        }
        // The export only read the dataset while it was closed, so hand it back unchanged.
        self.migrator
            .abandon_export_after(
                intent.run_id,
                ActivationTarget {
                    dataset_id: intent.dataset_id,
                    generation: from_generation,
                },
                false,
                || {
                    abandon_cutover(&self.home, &intent)
                        .map_err(|error| MigrationError::Staging(error.to_string()))?;
                    install::discard_plan(&self.home)
                        .map_err(|error| MigrationError::Staging(error.to_string()))
                },
            )
            .await?;
        Ok(RecoveryOutcome::RolledBack)
    }

    /// Cancel before the dataset was retired. Afterwards the return can only finish.
    pub async fn abort(&self) -> Result<RecoveryOutcome, CutoverError> {
        let Some(intent) = read_cutover(&self.home).map_err(authority)? else {
            return Ok(RecoveryOutcome::Idle);
        };
        match self.recover().await? {
            RecoveryOutcome::RolledForward { .. } => Err(CutoverError::AlreadyActivated),
            outcome @ (RecoveryOutcome::RolledBack | RecoveryOutcome::Idle) => {
                let _ = intent;
                Ok(outcome)
            }
        }
    }
}
