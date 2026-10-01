//! The server-owned control plane for storage: status, connection test, schema initialization,
//! plans, and the durable operation records behind them.
//!
//! One implementation serves the CLI, the app-server API and the TUI, so all three report the
//! same blocker codes, the same active backend and the same recovery actions. Credentials are
//! referenced, never carried: results contain codes and counts, not secrets or row contents.

mod journal;
mod ops;
mod ops_return;
mod plan;
mod service;
mod types;

pub use journal::OperationRecord;
pub use journal::OperationState;
pub use ops::Confirmation;
pub use ops::RecoveryKind;
pub use ops::RecoveryReport;
pub use plan::StoragePlan;
pub use service::StorageService;
pub use service::StorageServiceInputs;
pub use types::AuthorityLabel;
pub use types::BackendName;
pub use types::BlockerCode;
pub use types::CheckStage;
pub use types::ConnectionReport;
pub use types::PlanAction;
pub use types::RemoteSummary;
pub use types::StorageError;
pub use types::StorageStatus;

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
