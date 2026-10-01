//! Verified movement of a Codex store between SQLite and PostgreSQL.
//!
//! The engine reads every domain from a source, writes it to a target in resumable batches,
//! and proves the target matches by comparing a canonical digest of each domain. It never
//! switches the active backend; activation is a separate, explicit step.

mod attachments;
mod digest;
mod domain;
mod engine;
mod goals;
mod projects;
mod queue;
mod sections;
mod source;
mod threads;

pub use digest::DomainDigest;
pub use domain::Domain;
pub use engine::MigrationError;
pub use engine::Migrator;
pub use engine::RunSummary;
pub use engine::VerificationReport;
pub use source::SqliteSource;
