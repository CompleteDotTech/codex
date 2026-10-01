//! The `/storage` menu: where this host keeps Codex history, and the guided way to move it.
//!
//! Every fact on screen comes from the connected server, which acts on its own host's files and
//! credentials. The menu names that host, shows the active backend apart from a saved profile that
//! has not been proven, and never treats a selection as a finished migration. Secrets are never
//! entered or shown here; the saved profile refers to them by id.

use super::*;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::wrapping::word_wrap_lines;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::StorageActivateParams;
use codex_app_server_protocol::StorageActivateResponse;
use codex_app_server_protocol::StorageAuthority;
use codex_app_server_protocol::StorageBackend;
use codex_app_server_protocol::StorageBlocker;
use codex_app_server_protocol::StorageCancelParams;
use codex_app_server_protocol::StorageCancelResponse;
use codex_app_server_protocol::StorageCheckParams;
use codex_app_server_protocol::StorageCheckResponse;
use codex_app_server_protocol::StorageCheckStage;
use codex_app_server_protocol::StorageConnectionReport;
use codex_app_server_protocol::StorageOperation;
use codex_app_server_protocol::StorageOperationReadParams;
use codex_app_server_protocol::StorageOperationReadResponse;
use codex_app_server_protocol::StorageOperationState;
use codex_app_server_protocol::StoragePlan;
use codex_app_server_protocol::StoragePlanAction;
use codex_app_server_protocol::StoragePlanParams;
use codex_app_server_protocol::StoragePlanResponse;
use codex_app_server_protocol::StorageRecoverParams;
use codex_app_server_protocol::StorageRecoverResponse;
use codex_app_server_protocol::StorageStartParams;
use codex_app_server_protocol::StorageStartResponse;
use codex_app_server_protocol::StorageStatus;
use codex_app_server_protocol::StorageStatusParams;
use codex_app_server_protocol::StorageStatusResponse;
use ratatui::buffer::Buffer;
use ratatui::widgets::Paragraph;
use serde::de::DeserializeOwned;

struct StorageHeader(Vec<Line<'static>>);

impl Renderable for StorageHeader {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Renderable::render(
            &Paragraph::new(word_wrap_lines(&self.0, usize::from(area.width))),
            area,
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        word_wrap_lines(&self.0, usize::from(width)).len() as u16
    }
}

/// What a blocker means, in words an operator can act on.
pub(super) fn blocker_text(blocker: StorageBlocker) -> &'static str {
    match blocker {
        StorageBlocker::NoCandidateProfile => "No remote storage profile is saved on this host",
        StorageBlocker::MigratorCredentialMissing => {
            "The profile has no schema-owner credential for creating tables"
        }
        StorageBlocker::CredentialUnavailable => "The saved credential could not be read",
        StorageBlocker::CaCertificateRequired => "The profile needs a certificate authority file",
        StorageBlocker::UnsupportedNamespace => "The profile names a namespace Codex cannot use",
        StorageBlocker::ConnectionFailed => "The remote database could not be reached",
        StorageBlocker::ConnectionTimedOut => "The remote database did not answer in time",
        StorageBlocker::SchemaNeedsUpgrade => "The remote tables need to be upgraded first",
        StorageBlocker::SchemaTooNew => "The remote tables need a newer Codex",
        StorageBlocker::SchemaInvalid => "The remote tables are not usable",
        StorageBlocker::TargetNotEmpty => "The remote dataset already holds history",
        StorageBlocker::DatasetNotActivated => "The remote dataset was never activated",
        StorageBlocker::DatasetMismatch => "The remote dataset is not the one this host published",
        StorageBlocker::DatasetMigrating => "Another migration holds the remote dataset",
        StorageBlocker::DatasetRetired => "The remote dataset was handed back to local storage",
        StorageBlocker::CutoverInProgress => "A cutover was interrupted and needs recovery",
        StorageBlocker::AuthorityInvalid => "This host's storage records are unusable",
        StorageBlocker::AlreadyRemote => "History is already kept in PostgreSQL",
        StorageBlocker::NotRemote => "History is not kept in PostgreSQL",
        StorageBlocker::SqliteHomeDiffersFromCodexHome => {
            "The SQLite home differs from the Codex home"
        }
        StorageBlocker::StalePlan => "The preview is out of date; preview again",
        StorageBlocker::NotConfirmed => "The operation was not confirmed",
        StorageBlocker::OperationConflict => "The operation is not in a state that allows this",
        StorageBlocker::OperationNotFound => "No such operation",
        StorageBlocker::SourceUnreadable => "This host's history could not be read",
        StorageBlocker::StagingFailed => "The staged copy could not be written",
        StorageBlocker::VerificationFailed => "The copy did not match, so nothing was changed",
        StorageBlocker::StorageAdminRequired => "Only this host's own clients may change storage",
        StorageBlocker::Internal => "Storage hit an unexpected error",
    }
}

fn backend_text(backend: StorageBackend) -> &'static str {
    match backend {
        StorageBackend::LocalSqlite => "Local SQLite files",
        StorageBackend::RemotePostgres => "Remote PostgreSQL",
    }
}

fn authority_text(authority: StorageAuthority) -> &'static str {
    match authority {
        StorageAuthority::Unmanaged => "local, not yet under storage management",
        StorageAuthority::Local => "local",
        StorageAuthority::Remote => "remote",
        StorageAuthority::CutoverInProgress => "interrupted mid-switch",
        StorageAuthority::Invalid => "unknown; the storage records are unreadable",
    }
}

fn action_text(action: StoragePlanAction) -> &'static str {
    match action {
        StoragePlanAction::Migrate => "Migrate this host's history to PostgreSQL",
        StoragePlanAction::Return => "Return PostgreSQL history to this host's local files",
    }
}

fn state_text(state: StorageOperationState) -> &'static str {
    match state {
        StorageOperationState::Planned => "queued",
        StorageOperationState::Copying => "copying",
        StorageOperationState::Verifying => "verifying the copy",
        StorageOperationState::Ready => "verified and waiting for your go-ahead",
        StorageOperationState::Committing => "switching over",
        StorageOperationState::Active => "finished; the new storage is authoritative",
        StorageOperationState::Failed => "failed; nothing was switched",
        StorageOperationState::Cancelled => "cancelled; nothing was switched",
    }
}

/// A line that names the blocker and says whether another try can help.
fn blocker_line(blocker: StorageBlocker) -> Line<'static> {
    Line::from(vec!["  ✗ ".red(), blocker_text(blocker).into()])
}

fn back_item() -> SelectionItem {
    SelectionItem {
        name: "Back to storage".to_string(),
        actions: vec![Box::new(|tx| tx.send(AppEvent::OpenStorageMenu))],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn close_item() -> SelectionItem {
    SelectionItem {
        name: "Close".to_string(),
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn view(header: Vec<Line<'static>>, items: Vec<SelectionItem>) -> SelectionViewParams {
    SelectionViewParams {
        header: Box::new(StorageHeader(header)),
        items,
        ..SelectionViewParams::picker()
    }
}

/// The menu for a server that answered `storage/status`.
pub(super) fn status_view(status: &StorageStatus) -> SelectionViewParams {
    let mut header = vec![
        Line::from("Storage".bold()),
        Line::from(format!("Host: {}", status.host).dim()),
    ];
    let active = format!("Active: {}", backend_text(status.active_backend));
    header.push(match status.active_backend {
        StorageBackend::LocalSqlite => Line::from(active.cyan().bold()),
        StorageBackend::RemotePostgres => Line::from(active.green().bold()),
    });
    header.push(Line::from(
        format!("History: {}", authority_text(status.authority)).dim(),
    ));
    if status.candidate_configured && status.active_backend == StorageBackend::RemotePostgres {
        header.push(Line::from(
            "Connected through the remote profile saved on this host.".dim(),
        ));
    } else if status.candidate_configured {
        header.push(Line::from(
            "A remote profile is saved. It is only a proposal until a migration finishes.".dim(),
        ));
    } else {
        header.push(Line::from("No remote profile is saved on this host.".dim()));
    }
    header.extend(status.blockers.iter().copied().map(blocker_line));

    let mut items = Vec::new();
    match status.authority {
        StorageAuthority::CutoverInProgress => items.push(SelectionItem {
            name: "Recover the interrupted cutover".to_string(),
            description: Some("Finish or undo it from what both sides recorded".to_string()),
            actions: vec![Box::new(|tx| tx.send(AppEvent::StorageRecoverRequested))],
            dismiss_on_select: true,
            ..Default::default()
        }),
        StorageAuthority::Invalid => {}
        StorageAuthority::Unmanaged | StorageAuthority::Local | StorageAuthority::Remote => {
            let remote = status.active_backend == StorageBackend::RemotePostgres;
            items.push(SelectionItem {
                name: "Test the remote connection".to_string(),
                description: Some("Checks the saved profile; changes nothing".to_string()),
                is_disabled: !status.candidate_configured,
                disabled_reason: (!status.candidate_configured)
                    .then(|| "Save a remote profile on this host first".to_string()),
                actions: vec![Box::new(|tx| tx.send(AppEvent::StorageCheckRequested))],
                dismiss_on_select: true,
                ..Default::default()
            });
            let action = if remote {
                StoragePlanAction::Return
            } else {
                StoragePlanAction::Migrate
            };
            items.push(SelectionItem {
                name: if remote {
                    "Preview returning to local files".to_string()
                } else {
                    "Preview migrating to PostgreSQL".to_string()
                },
                description: Some("Shows what would move and what blocks it".to_string()),
                is_disabled: !remote && !status.candidate_configured,
                disabled_reason: (!remote && !status.candidate_configured)
                    .then(|| "Save a remote profile on this host first".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::StoragePlanRequested(action))
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
    }
    items.push(close_item());
    view(header, items)
}

/// The message for a server that cannot answer: too old, unreachable, or not allowed.
pub(super) fn unavailable_view(message: &str) -> SelectionViewParams {
    view(
        vec![
            Line::from("Storage".bold()),
            Line::from(vec!["✗ ".red(), "Storage is unavailable".into()]),
            Line::from(message.to_string().dim()),
        ],
        vec![back_item(), close_item()],
    )
}

pub(super) fn check_view(report: &StorageConnectionReport) -> SelectionViewParams {
    let stage = match report.stage {
        StorageCheckStage::Profile => "the saved profile",
        StorageCheckStage::Credential => "reading the credential",
        StorageCheckStage::Connect => "connecting",
        StorageCheckStage::Schema => "checking the tables",
        StorageCheckStage::Dataset => "reading the dataset",
        StorageCheckStage::Ready => "every step",
    };
    let mut header = vec![Line::from("Remote connection test".bold())];
    match report.blocker {
        Some(blocker) => {
            header.push(Line::from(format!("Stopped at {stage}.").dim()));
            header.push(blocker_line(blocker));
        }
        None => {
            header.push(Line::from(vec![
                "✓ ".green(),
                "The remote dataset is reachable".into(),
            ]));
            if let Some(state) = &report.dataset_state {
                header.push(Line::from(format!("Dataset state: {state}").dim()));
            }
            match report.empty {
                Some(true) => header.push(Line::from("It holds no history yet.".dim())),
                Some(false) => header.push(Line::from("It already holds history.".dim())),
                None => {}
            }
        }
    }
    view(header, vec![back_item(), close_item()])
}

fn count_line(label: &str, count: i64) -> Option<Line<'static>> {
    (count > 0).then(|| Line::from(format!("  {label}: {count}").dim()))
}

pub(super) fn plan_view(plan: &StoragePlan) -> SelectionViewParams {
    let mut header = vec![
        Line::from(action_text(plan.action).bold()),
        Line::from(format!("Host: {}", plan.host).dim()),
    ];
    if !plan.destination.is_empty() {
        header.push(Line::from(
            format!("Destination: {}", plan.destination).dim(),
        ));
    }
    if let Some(estimate) = &plan.estimate {
        header.push(Line::from("This host holds:".dim()));
        header.extend(
            [
                count_line("threads", estimate.threads),
                count_line("projects", estimate.projects),
                count_line("attachments", estimate.attachments),
                count_line("queued messages", estimate.queued_items),
                count_line("goals", estimate.goals),
                count_line("log rows", estimate.logs),
                count_line("rollout files", estimate.rollout_files),
            ]
            .into_iter()
            .flatten(),
        );
    }
    header.extend(plan.blockers.iter().copied().map(blocker_line));
    let mut items = Vec::new();
    if plan.blockers.is_empty() {
        header.push(Line::from(
            "Stop every other Codex process on this host before you start.".bold(),
        ));
        let action = plan.action;
        let plan_id = plan.plan_id.clone();
        items.push(SelectionItem {
            name: "Start now: other Codex processes are stopped".to_string(),
            description: Some("Copies and verifies; you decide when to switch".to_string()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::StorageStartRequested {
                    action,
                    plan_id: plan_id.clone(),
                });
            })],
            require_explicit_confirmation: true,
            dismiss_on_select: true,
            ..Default::default()
        });
    }
    items.push(back_item());
    items.push(close_item());
    view(header, items)
}

pub(super) fn operation_view(operation: &StorageOperation) -> SelectionViewParams {
    let mut header = vec![
        Line::from(action_text(operation.action).bold()),
        Line::from(format!("Operation {}", operation.operation_id).dim()),
        Line::from(format!("State: {}", state_text(operation.state))),
    ];
    for copied in &operation.copied {
        header.push(Line::from(
            format!("  {}: {} rows", copied.domain, copied.rows).dim(),
        ));
    }
    if let Some(blocker) = operation.blocker {
        header.push(blocker_line(blocker));
    }
    let mut items = Vec::new();
    let operation_id = operation.operation_id.clone();
    let refresh_id = operation_id.clone();
    if matches!(
        operation.state,
        StorageOperationState::Planned
            | StorageOperationState::Copying
            | StorageOperationState::Verifying
            | StorageOperationState::Committing
    ) {
        items.push(SelectionItem {
            name: "Refresh progress".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::StorageOperationRefreshRequested {
                    operation_id: refresh_id.clone(),
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
    }
    if operation.state == StorageOperationState::Ready {
        let activate_id = operation_id.clone();
        items.push(SelectionItem {
            name: "Switch to the verified copy".to_string(),
            description: Some(
                "Makes the copy authoritative; this host stops using the old one".to_string(),
            ),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::StorageActivateRequested {
                    operation_id: activate_id.clone(),
                });
            })],
            require_explicit_confirmation: true,
            dismiss_on_select: true,
            ..Default::default()
        });
    }
    if !matches!(
        operation.state,
        StorageOperationState::Active | StorageOperationState::Cancelled
    ) {
        let cancel_id = operation_id;
        items.push(SelectionItem {
            name: "Cancel this operation".to_string(),
            description: Some("Keeps the current storage; nothing is deleted".to_string()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::StorageCancelRequested {
                    operation_id: cancel_id.clone(),
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
    }
    items.push(back_item());
    items.push(close_item());
    view(header, items)
}

/// The blocker a server error carries, or its plain message.
fn describe(error: &TypedRequestError) -> String {
    match error {
        TypedRequestError::Server { source, .. } => source
            .data
            .as_ref()
            .and_then(|data| data.get("blocker"))
            .and_then(|code| serde_json::from_value::<StorageBlocker>(code.clone()).ok())
            .map_or_else(
                || {
                    "This server does not offer storage management, or refused the request."
                        .to_string()
                },
                |blocker| blocker_text(blocker).to_string(),
            ),
        TypedRequestError::Transport { .. } => "The connection to the server was lost.".to_string(),
        TypedRequestError::Deserialize { .. } => {
            "The server sent an answer this version cannot read.".to_string()
        }
    }
}

async fn call<T: DeserializeOwned>(
    handle: AppServerRequestHandle,
    request: impl FnOnce(RequestId) -> ClientRequest,
) -> Result<T, String> {
    let id = RequestId::String(format!("storage-{}", uuid::Uuid::new_v4()));
    handle
        .request_typed(request(id))
        .await
        .map_err(|error| describe(&error))
}

impl App {
    fn storage_task<T: Send + 'static>(
        &self,
        app_server: &AppServerSession,
        run: impl FnOnce(
            AppServerRequestHandle,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>
        + Send
        + 'static,
        deliver: impl FnOnce(T) -> AppEvent + Send + 'static,
    ) {
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            tx.send(deliver(run(handle).await));
        });
    }

    pub(super) fn request_storage_status(&mut self, app_server: &AppServerSession) {
        self.storage_task(
            app_server,
            |handle| {
                Box::pin(async move {
                    call::<StorageStatusResponse>(handle, |request_id| {
                        ClientRequest::StorageStatus {
                            request_id,
                            params: StorageStatusParams {
                                probe_remote: false,
                            },
                        }
                    })
                    .await
                    .map(|response| response.status)
                })
            },
            AppEvent::StorageStatusLoaded,
        );
    }

    pub(super) fn show_storage_status(&mut self, result: Result<StorageStatus, String>) {
        let params = match result {
            Ok(status) => status_view(&status),
            Err(message) => unavailable_view(&message),
        };
        self.chat_widget.show_selection_view(params);
    }

    pub(super) fn request_storage_check(&mut self, app_server: &AppServerSession) {
        self.storage_task(
            app_server,
            |handle| {
                Box::pin(async move {
                    call::<StorageCheckResponse>(handle, |request_id| ClientRequest::StorageCheck {
                        request_id,
                        params: StorageCheckParams {},
                    })
                    .await
                    .map(|response| response.report)
                })
            },
            AppEvent::StorageCheckLoaded,
        );
    }

    pub(super) fn show_storage_check(&mut self, result: Result<StorageConnectionReport, String>) {
        let params = match result {
            Ok(report) => check_view(&report),
            Err(message) => unavailable_view(&message),
        };
        self.chat_widget.show_selection_view(params);
    }

    pub(super) fn request_storage_plan(
        &mut self,
        app_server: &AppServerSession,
        action: StoragePlanAction,
    ) {
        self.storage_task(
            app_server,
            move |handle| {
                Box::pin(async move {
                    call::<StoragePlanResponse>(handle, |request_id| ClientRequest::StoragePlan {
                        request_id,
                        params: StoragePlanParams { action },
                    })
                    .await
                    .map(|response| response.plan)
                })
            },
            AppEvent::StoragePlanLoaded,
        );
    }

    pub(super) fn show_storage_plan(&mut self, result: Result<StoragePlan, String>) {
        let params = match result {
            Ok(plan) => plan_view(&plan),
            Err(message) => unavailable_view(&message),
        };
        self.chat_widget.show_selection_view(params);
    }

    pub(super) fn request_storage_start(
        &mut self,
        app_server: &AppServerSession,
        action: StoragePlanAction,
        plan_id: String,
    ) {
        self.storage_task(
            app_server,
            move |handle| {
                Box::pin(async move {
                    call::<StorageStartResponse>(handle, |request_id| ClientRequest::StorageStart {
                        request_id,
                        params: StorageStartParams {
                            action,
                            plan_id,
                            operation_id: None,
                            writers_stopped: true,
                            activate: false,
                        },
                    })
                    .await
                    .map(|response| response.operation)
                })
            },
            AppEvent::StorageOperationLoaded,
        );
    }

    pub(super) fn request_storage_activate(
        &mut self,
        app_server: &AppServerSession,
        operation_id: String,
    ) {
        self.storage_task(
            app_server,
            move |handle| {
                Box::pin(async move {
                    call::<StorageActivateResponse>(handle, |request_id| {
                        ClientRequest::StorageActivate {
                            request_id,
                            params: StorageActivateParams { operation_id },
                        }
                    })
                    .await
                    .map(|response| response.operation)
                })
            },
            AppEvent::StorageOperationLoaded,
        );
    }

    pub(super) fn request_storage_cancel(
        &mut self,
        app_server: &AppServerSession,
        operation_id: String,
    ) {
        self.storage_task(
            app_server,
            move |handle| {
                Box::pin(async move {
                    call::<StorageCancelResponse>(handle, |request_id| {
                        ClientRequest::StorageCancel {
                            request_id,
                            params: StorageCancelParams { operation_id },
                        }
                    })
                    .await
                    .map(|response| response.operation)
                })
            },
            AppEvent::StorageOperationLoaded,
        );
    }

    pub(super) fn request_storage_operation(
        &mut self,
        app_server: &AppServerSession,
        operation_id: String,
    ) {
        self.storage_task(
            app_server,
            move |handle| {
                Box::pin(async move {
                    call::<StorageOperationReadResponse>(handle, |request_id| {
                        ClientRequest::StorageOperationRead {
                            request_id,
                            params: StorageOperationReadParams { operation_id },
                        }
                    })
                    .await
                    .map(|response| response.operation)
                })
            },
            AppEvent::StorageOperationLoaded,
        );
    }

    pub(super) fn request_storage_recover(&mut self, app_server: &AppServerSession) {
        self.storage_task(
            app_server,
            |handle| {
                Box::pin(async move {
                    call::<StorageRecoverResponse>(handle, |request_id| {
                        ClientRequest::StorageRecover {
                            request_id,
                            params: StorageRecoverParams {},
                        }
                    })
                    .await
                    .map(|_| ())
                })
            },
            |result| match result {
                // Whatever recovery decided, the status view shows where things stand now.
                Ok(()) => AppEvent::OpenStorageMenu,
                Err(message) => AppEvent::StorageStatusLoaded(Err(message)),
            },
        );
    }

    pub(super) fn show_storage_operation(&mut self, result: Result<StorageOperation, String>) {
        let params = match result {
            Ok(operation) => operation_view(&operation),
            Err(message) => unavailable_view(&message),
        };
        self.chat_widget.show_selection_view(params);
    }
}

#[cfg(test)]
#[path = "storage_menu_tests.rs"]
mod tests;
