//! Thin adapters from the thread-store trait types to the catalog for sections, attachments and
//! projects. They map errors exactly as the local store does, so callers see one contract.

use codex_postgres_thread_catalog::PostgresThreadCatalog;
use codex_protocol::ThreadId;
use codex_thread_store::AddThreadAttachmentOutcome;
use codex_thread_store::AddThreadAttachmentParams;
use codex_thread_store::CreateProjectParams;
use codex_thread_store::CreateThreadSectionParams;
use codex_thread_store::CreatedProject;
use codex_thread_store::DeleteThreadSectionParams;
use codex_thread_store::DeletedProject;
use codex_thread_store::ListProjectsParams;
use codex_thread_store::ListThreadAttachmentsParams;
use codex_thread_store::ListThreadSectionsParams;
use codex_thread_store::MoveProjectParams;
use codex_thread_store::ProjectMoveOutcome;
use codex_thread_store::RemoveThreadAttachmentOutcome;
use codex_thread_store::RemoveThreadAttachmentParams;
use codex_thread_store::RenameThreadSectionParams;
use codex_thread_store::SortDirection;
use codex_thread_store::StoredProject;
use codex_thread_store::StoredProjectRoot;
use codex_thread_store::StoredProjectsPage;
use codex_thread_store::StoredThreadSection;
use codex_thread_store::StoredThreadSectionsPage;
use codex_thread_store::ThreadAttachmentPage;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use codex_thread_store::UpdateProjectParams;
use codex_thread_store::UpdatedProject;

fn stored_section(section: codex_state::ThreadSection) -> StoredThreadSection {
    StoredThreadSection {
        id: section.id,
        name: section.name,
        appearance: section.appearance,
    }
}

fn section_error(operation: &str, error: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("failed to {operation} thread section: {error}"),
    }
}

pub(crate) async fn list_thread_sections(
    catalog: &PostgresThreadCatalog,
    params: ListThreadSectionsParams,
) -> ThreadStoreResult<StoredThreadSectionsPage> {
    let page = catalog
        .list_thread_sections(params.cursor.as_deref(), params.limit)
        .await
        .map_err(|err| section_error("list", err))?;
    Ok(StoredThreadSectionsPage {
        sections: page.sections.into_iter().map(stored_section).collect(),
        next_cursor: page.next_cursor,
    })
}

pub(crate) async fn create_thread_section(
    catalog: &PostgresThreadCatalog,
    params: CreateThreadSectionParams,
) -> ThreadStoreResult<StoredThreadSection> {
    catalog
        .create_thread_section(&params.name, params.appearance)
        .await
        .map(stored_section)
        .map_err(|err| section_error("create", err))
}

pub(crate) async fn rename_thread_section(
    catalog: &PostgresThreadCatalog,
    params: RenameThreadSectionParams,
) -> ThreadStoreResult<Option<StoredThreadSection>> {
    catalog
        .rename_thread_section(&params.section_id, &params.name, params.appearance)
        .await
        .map(|section| section.map(stored_section))
        .map_err(|err| section_error("update", err))
}

pub(crate) async fn delete_thread_section(
    catalog: &PostgresThreadCatalog,
    params: DeleteThreadSectionParams,
) -> ThreadStoreResult<bool> {
    catalog
        .delete_thread_section(&params.section_id)
        .await
        .map_err(|err| section_error("delete", err))
}

fn attachment_error(
    operation: &str,
    thread_id: Option<ThreadId>,
    error: impl std::fmt::Display,
) -> ThreadStoreError {
    let message = error.to_string();
    if let Some(message) = message.strip_prefix("invalid thread attachment request: ") {
        return ThreadStoreError::InvalidRequest {
            message: message.to_string(),
        };
    }
    if message.starts_with("thread not found: ")
        && let Some(thread_id) = thread_id
    {
        return ThreadStoreError::ThreadNotFound { thread_id };
    }
    ThreadStoreError::Internal {
        message: format!("failed to {operation} thread attachment: {message}"),
    }
}

pub(crate) async fn copy_thread_attachments(
    catalog: &PostgresThreadCatalog,
    source: ThreadId,
    destination: ThreadId,
) -> ThreadStoreResult<()> {
    catalog
        .copy_thread_attachments(source, destination)
        .await
        .map_err(|error| attachment_error("copy", Some(destination), error))
}

pub(crate) async fn add_thread_attachment(
    catalog: &PostgresThreadCatalog,
    params: AddThreadAttachmentParams,
) -> ThreadStoreResult<AddThreadAttachmentOutcome> {
    catalog
        .add_thread_attachment(
            params.thread_id,
            &params.attachment_type,
            &params.identity_key,
            &params.payload,
        )
        .await
        .map_err(|error| attachment_error("add", Some(params.thread_id), error))
}

pub(crate) async fn list_thread_attachments(
    catalog: &PostgresThreadCatalog,
    params: ListThreadAttachmentsParams,
) -> ThreadStoreResult<ThreadAttachmentPage> {
    catalog
        .list_thread_attachments(params.thread_id, params.cursor.as_deref(), params.limit)
        .await
        .map_err(|error| attachment_error("list", /*thread_id*/ None, error))
}

pub(crate) async fn remove_thread_attachment(
    catalog: &PostgresThreadCatalog,
    params: RemoveThreadAttachmentParams,
) -> ThreadStoreResult<RemoveThreadAttachmentOutcome> {
    catalog
        .remove_thread_attachment(
            params.thread_id,
            &params.attachment_type,
            &params.identity_key,
        )
        .await
        .map_err(|error| attachment_error("remove", Some(params.thread_id), error))
}

fn internal(error: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: error.to_string(),
    }
}

fn state_root(root: StoredProjectRoot) -> codex_state::ProjectRoot {
    codex_state::ProjectRoot { path: root.path }
}

fn stored_project(project: codex_state::Project) -> StoredProject {
    StoredProject {
        id: project.id,
        name: project.name,
        roots: project
            .roots
            .into_iter()
            .map(|root| StoredProjectRoot { path: root.path })
            .collect(),
        metadata: project.metadata,
        position: project.position,
        created_at_ms: project.created_at_ms,
        updated_at_ms: project.updated_at_ms,
        recency_at_ms: project.recency_at_ms,
    }
}

pub(crate) async fn list_projects(
    catalog: &PostgresThreadCatalog,
    params: ListProjectsParams,
) -> ThreadStoreResult<StoredProjectsPage> {
    let has_cursor = params.cursor.is_some();
    let page = catalog
        .list_projects(
            params.cursor.as_deref(),
            params.limit,
            params.sort_key,
            match params.sort_direction {
                SortDirection::Asc => codex_state::SortDirection::Asc,
                SortDirection::Desc => codex_state::SortDirection::Desc,
            },
        )
        .await
        .map_err(|error| {
            if has_cursor && error.to_string().starts_with("invalid project cursor:") {
                ThreadStoreError::InvalidRequest {
                    message: error.to_string(),
                }
            } else {
                internal(error)
            }
        })?;
    Ok(StoredProjectsPage {
        projects: page.projects.into_iter().map(stored_project).collect(),
        next_cursor: page.next_cursor,
    })
}

pub(crate) async fn read_project(
    catalog: &PostgresThreadCatalog,
    project_id: String,
) -> ThreadStoreResult<Option<StoredProject>> {
    catalog
        .get_project(&project_id)
        .await
        .map(|project| project.map(stored_project))
        .map_err(internal)
}

pub(crate) async fn create_project(
    catalog: &PostgresThreadCatalog,
    params: CreateProjectParams,
) -> ThreadStoreResult<CreatedProject> {
    catalog
        .create_project(
            params.name,
            params.roots.into_iter().map(state_root).collect(),
            params.metadata,
            &params.thread_ids,
            &params.idempotency_key,
        )
        .await
        .map(|created| CreatedProject {
            project: stored_project(created.project),
            created: created.created,
        })
        .map_err(|error| {
            let message = error.to_string();
            if message.starts_with("idempotency key refers to deleted project:") {
                ThreadStoreError::InvalidRequest { message }
            } else {
                internal(message)
            }
        })
}

pub(crate) async fn update_project(
    catalog: &PostgresThreadCatalog,
    params: UpdateProjectParams,
) -> ThreadStoreResult<Option<UpdatedProject>> {
    catalog
        .update_project(
            &params.project_id,
            params.name,
            params
                .roots
                .map(|roots| roots.into_iter().map(state_root).collect()),
            params.metadata,
        )
        .await
        .map(|result| {
            result.map(|(project, changed)| UpdatedProject {
                project: stored_project(project),
                changed,
            })
        })
        .map_err(internal)
}

pub(crate) async fn move_project(
    catalog: &PostgresThreadCatalog,
    params: MoveProjectParams,
) -> ThreadStoreResult<Option<ProjectMoveOutcome>> {
    catalog
        .move_project(&params.project_id, params.before_project_id.as_deref())
        .await
        .map(|result| {
            result.map(|changed| {
                if changed {
                    ProjectMoveOutcome::Moved
                } else {
                    ProjectMoveOutcome::Unchanged
                }
            })
        })
        .map_err(|error| {
            let message = error.to_string();
            if message.starts_with("before project ")
                || message.starts_with("project ") && message.contains("cannot be moved")
            {
                ThreadStoreError::InvalidRequest { message }
            } else {
                internal(message)
            }
        })
}

pub(crate) async fn delete_project(
    catalog: &PostgresThreadCatalog,
    project_id: String,
) -> ThreadStoreResult<Option<DeletedProject>> {
    catalog
        .delete_project(&project_id)
        .await
        .map(|result| {
            result.map(
                |(affected_active_thread_ids, affected_archived_thread_ids)| DeletedProject {
                    affected_active_thread_ids,
                    affected_archived_thread_ids,
                },
            )
        })
        .map_err(internal)
}
