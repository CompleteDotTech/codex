//! Verified movement of a Codex store between SQLite and PostgreSQL.
//!
//! The engine reads every domain from a source, writes it to a target in resumable batches,
//! and proves the target matches by comparing a canonical digest of each domain. It never
//! switches the active backend; activation is a separate, explicit step.

mod attachments;
mod board;
mod cutover;
mod digest;
mod domain;
mod engine;
mod external_imports;
mod goals;
mod logs;
mod memory;
mod projects;
mod queue;
mod rollouts;
mod sections;
mod source;
mod threads;

pub use cutover::Cutover;
pub use cutover::CutoverError;
pub use cutover::CutoverStatus;
pub use cutover::RecoveryOutcome;
pub use digest::DomainDigest;
pub use domain::Domain;
pub use engine::ActivationState;
pub use engine::ActivationTarget;
pub use engine::MigrationError;
pub use engine::Migrator;
pub use engine::RunSummary;
pub use engine::VerificationReport;
pub use source::SqliteSource;
