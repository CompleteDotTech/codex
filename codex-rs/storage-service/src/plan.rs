//! What an action would do, and what stops it, decided without changing anything.
//!
//! A plan is identified by a digest of the facts it was built from. Starting an operation
//! rebuilds the plan and compares digests, so a confirmation given for one state of the world
//! can never start work against another.

use crate::BlockerCode;
use crate::types::ConnectionReport;
use crate::types::PlanAction;
use codex_storage_authority::AuthorityError;
use codex_storage_authority::AuthorityState;
use codex_storage_authority::RemotePostgresProfile;
use codex_storage_migration::SourceEstimate;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use uuid::Uuid;

/// The facts a plan is built from.
pub(crate) struct PlanInputs {
    pub(crate) authority: Result<AuthorityState, AuthorityError>,
    pub(crate) connection: ConnectionReport,
    pub(crate) estimate: Option<SourceEstimate>,
    pub(crate) sqlite_home_matches: bool,
}

/// A preview that is either startable or lists exactly what blocks it.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StoragePlan {
    pub plan_id: Uuid,
    pub action: PlanAction,
    /// Changes whenever any fact behind the plan changes.
    pub digest: String,
    /// The machine whose storage the plan would change.
    pub host: String,
    /// `endpoint:port/database/namespace`; never a credential.
    pub destination: String,
    pub local_generation: Option<u64>,
    pub estimate: Option<SourceEstimate>,
    pub connection: ConnectionReport,
    pub blockers: Vec<BlockerCode>,
    /// Local writers must be stopped before the operation starts.
    pub requires_pause: bool,
}

impl StoragePlan {
    pub fn is_startable(&self) -> bool {
        self.blockers.is_empty()
    }

    pub(crate) fn build(
        action: PlanAction,
        host: &str,
        profile: Option<&RemotePostgresProfile>,
        inputs: PlanInputs,
    ) -> Self {
        let destination = profile.map_or_else(String::new, |profile| {
            format!(
                "{}:{}/{}/{}",
                profile.endpoint(),
                profile.port(),
                profile.database(),
                profile.namespace()
            )
        });
        let mut blockers = Vec::new();
        let (authority_label, generation, local_dataset) = match &inputs.authority {
            Ok(AuthorityState::Unmanaged) => ("unmanaged", None, None),
            Ok(AuthorityState::Local(local)) => (
                "local",
                Some(local.identity.generation),
                Some(local.identity.dataset_id),
            ),
            Ok(AuthorityState::Remote(local)) => (
                "remote",
                Some(local.identity.generation),
                Some(local.identity.dataset_id),
            ),
            Ok(AuthorityState::CutoverInProgress(_)) => ("cutover", None, None),
            Err(_) => ("invalid", None, None),
        };
        match (action, &inputs.authority) {
            (_, Err(_)) => blockers.push(BlockerCode::AuthorityInvalid),
            (_, Ok(AuthorityState::CutoverInProgress(_))) => {
                blockers.push(BlockerCode::CutoverInProgress);
            }
            (PlanAction::Migrate, Ok(AuthorityState::Remote(_))) => {
                blockers.push(BlockerCode::AlreadyRemote);
            }
            (PlanAction::Return, Ok(AuthorityState::Unmanaged | AuthorityState::Local(_))) => {
                blockers.push(BlockerCode::NotRemote);
            }
            (PlanAction::Migrate, Ok(AuthorityState::Unmanaged | AuthorityState::Local(_)))
            | (PlanAction::Return, Ok(AuthorityState::Remote(_))) => {}
        }
        if !inputs.sqlite_home_matches {
            blockers.push(BlockerCode::SqliteHomeDiffersFromCodexHome);
        }
        if let Some(code) = inputs.connection.blocker {
            blockers.push(code);
        } else {
            match inputs.connection.dataset_state.as_deref() {
                Some("migrating") => blockers.push(BlockerCode::DatasetMigrating),
                Some("retired") => blockers.push(BlockerCode::DatasetRetired),
                _ => {}
            }
            match action {
                PlanAction::Migrate => {
                    if inputs.connection.empty != Some(true) {
                        blockers.push(BlockerCode::TargetNotEmpty);
                    }
                }
                PlanAction::Return => {
                    if inputs.connection.dataset_id.is_none() {
                        blockers.push(BlockerCode::DatasetNotActivated);
                    } else if inputs.connection.dataset_id != local_dataset {
                        blockers.push(BlockerCode::DatasetMismatch);
                    }
                }
            }
        }
        blockers.dedup();
        let facts = serde_json::json!({
            "action": action,
            "host": host,
            "destination": destination,
            "authority": authority_label,
            "generation": generation,
            "dataset": local_dataset,
            "connection": inputs.connection,
            "estimate": inputs.estimate,
            "sqlite_home_matches": inputs.sqlite_home_matches,
        });
        let digest: String = Sha256::digest(facts.to_string().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let plan_id = Uuid::from_slice(&Sha256::digest(digest.as_bytes())[..16])
            .unwrap_or_else(|_| Uuid::nil());
        Self {
            plan_id,
            action,
            digest,
            host: host.to_string(),
            destination,
            local_generation: generation,
            estimate: inputs.estimate,
            connection: inputs.connection,
            blockers,
            requires_pause: true,
        }
    }
}
