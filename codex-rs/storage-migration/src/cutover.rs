//! Moving authority to the verified PostgreSQL copy without ever having two writable histories.
//!
//! The protocol writes the host's intent first, publishes the new generation in PostgreSQL
//! second, and rewrites the host's authority records last. A crash anywhere leaves evidence on
//! both sides, and [`Cutover::recover`] decides from that evidence alone whether to finish the
//! move or undo it. Nothing here deletes source data.

use crate::ActivationState;
use crate::ActivationTarget;
use crate::MigrationError;
use crate::Migrator;
use codex_storage_authority::ActiveBackend;
use codex_storage_authority::AuthorityError;
use codex_storage_authority::CutoverIntent;
use codex_storage_authority::LocalAuthority;
use codex_storage_authority::abandon_cutover;
use codex_storage_authority::begin_cutover;
use codex_storage_authority::complete_cutover;
use codex_storage_authority::load_authority;
use codex_storage_authority::read_cutover;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// Failures never carry paths, connection details or row contents.
#[derive(Debug, Error)]
pub enum CutoverError {
    #[error("the local authority records refused the change: {0}")]
    Authority(String),
    #[error("the destination refused the change: {0}")]
    Destination(#[from] MigrationError),
    #[error("the destination published a different generation; the cutover cannot be reconciled")]
    Conflict,
    #[error("the outcome is unknown; run recovery to settle it")]
    Uncertain,
    #[error("authority already moved to PostgreSQL; use the reverse migration to leave it")]
    AlreadyActivated,
}

fn authority(error: AuthorityError) -> CutoverError {
    CutoverError::Authority(error.to_string())
}

/// How recovery settled an interrupted cutover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No cutover was in progress.
    Idle,
    /// The destination had published, so the local records were moved to match it.
    RolledForward { generation: u64 },
    /// The destination never published, so the intent was removed and the source stays
    /// authoritative.
    RolledBack,
}

/// What both sides currently say, for a status display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutoverStatus {
    pub intent: Option<CutoverIntent>,
    pub local_generation: Option<u64>,
    pub local_backend: Option<ActiveBackend>,
    pub remote: ActivationState,
}

/// The cutover for one home and one destination.
pub struct Cutover {
    home: PathBuf,
    migrator: Migrator,
}

impl Cutover {
    pub fn new(home: PathBuf, migrator: Migrator) -> Self {
        Self { home, migrator }
    }

    /// Record the intent. Nothing else changes yet.
    pub fn prepare(&self, run_id: Uuid) -> Result<CutoverIntent, CutoverError> {
        begin_cutover(&self.home, run_id, ActiveBackend::Remote).map_err(authority)
    }

    /// Publish the intent's generation in PostgreSQL. Safe to repeat.
    pub async fn publish(&self, intent: &CutoverIntent) -> Result<(), CutoverError> {
        let generation = i64::try_from(intent.to_generation).map_err(|_| CutoverError::Conflict)?;
        self.migrator
            .activate(
                intent.run_id,
                ActivationTarget {
                    dataset_id: intent.dataset_id,
                    generation,
                },
            )
            .await?;
        Ok(())
    }

    /// Move the local records to the intent's generation. Safe to repeat.
    pub fn finish(&self, intent: &CutoverIntent) -> Result<LocalAuthority, CutoverError> {
        complete_cutover(&self.home, intent).map_err(authority)
    }

    /// Run the whole protocol for a verified run.
    ///
    /// A refusal before the destination changed undoes the intent. An unclear outcome keeps it
    /// and returns [`CutoverError::Uncertain`] so recovery can look at both sides.
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
                return Err(CutoverError::Destination(refusal));
            }
            Err(_) => return Err(CutoverError::Uncertain),
        }
        self.finish(&intent)
    }

    /// Settle an interrupted cutover from the evidence on both sides.
    pub async fn recover(&self) -> Result<RecoveryOutcome, CutoverError> {
        let Some(intent) = read_cutover(&self.home).map_err(authority)? else {
            return Ok(RecoveryOutcome::Idle);
        };
        let remote = self.migrator.activation_state().await?;
        let to_generation =
            i64::try_from(intent.to_generation).map_err(|_| CutoverError::Conflict)?;
        if !remote.migrating
            && remote.generation == to_generation
            && remote.dataset_id == Some(intent.dataset_id)
        {
            let moved = self.finish(&intent)?;
            return Ok(RecoveryOutcome::RolledForward {
                generation: moved.identity.generation,
            });
        }
        if remote.generation >= to_generation {
            // Someone published a generation this intent does not own.
            return Err(CutoverError::Conflict);
        }
        abandon_cutover(&self.home, &intent).map_err(authority)?;
        Ok(RecoveryOutcome::RolledBack)
    }

    /// Cancel before the destination published. After publication the only way back is the
    /// reverse migration, because remote writes may already exist.
    pub async fn abort(&self) -> Result<RecoveryOutcome, CutoverError> {
        let Some(intent) = read_cutover(&self.home).map_err(authority)? else {
            return match load_authority(&self.home)
                .map_err(authority)?
                .marker
                .active_backend
            {
                ActiveBackend::Remote => Err(CutoverError::AlreadyActivated),
                ActiveBackend::Local => Ok(RecoveryOutcome::Idle),
            };
        };
        match self.recover().await? {
            RecoveryOutcome::RolledForward { .. } => Err(CutoverError::AlreadyActivated),
            RecoveryOutcome::RolledBack => {
                self.migrator.abandon(intent.run_id).await?;
                Ok(RecoveryOutcome::RolledBack)
            }
            RecoveryOutcome::Idle => Ok(RecoveryOutcome::Idle),
        }
    }

    /// Both sides' view, without changing either.
    pub async fn status(&self) -> Result<CutoverStatus, CutoverError> {
        let intent = read_cutover(&self.home).map_err(authority)?;
        let local = load_authority(&self.home).ok();
        Ok(CutoverStatus {
            intent,
            local_generation: local.as_ref().map(|local| local.identity.generation),
            local_backend: local.as_ref().map(|local| local.marker.active_backend),
            remote: self.migrator.activation_state().await?,
        })
    }
}
