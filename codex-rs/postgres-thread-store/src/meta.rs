//! Conversions between catalog rows, thread metadata patches and `StoredThread`.
//!
//! These mirror the local store's pure conversions so both backends report the same fields.
//! Paths come from the catalog as recorded origins, so a stored thread never claims a local
//! rollout path.

use codex_git_utils::GitSha;
use codex_protocol::SanitizedGitUrl;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GitInfo;
use codex_protocol::protocol::NetworkAccess;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_state::ThreadMetadata;
use codex_thread_store::GitInfoPatch;
use codex_thread_store::StoredThread;
use codex_thread_store::ThreadMetadataPatch;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::path::PathBuf;

pub(crate) fn stored_thread_from_metadata(
    metadata: ThreadMetadata,
    default_model_provider_id: &str,
    parent_thread_id: Option<ThreadId>,
    forked_from_id: Option<ThreadId>,
) -> StoredThread {
    let history_mode = metadata.history_mode;
    let name = thread_name(&metadata);
    let preview = metadata
        .preview
        .clone()
        .or_else(|| metadata.first_user_message.clone())
        .unwrap_or_default();
    let permission_profile =
        permission_profile_from_metadata_value(&metadata.sandbox_policy, metadata.cwd.as_path());
    StoredThread {
        thread_id: metadata.id,
        extra_config: None,
        rollout_path: None,
        forked_from_id,
        parent_thread_id,
        preview,
        name,
        model_provider: if metadata.model_provider.is_empty() {
            default_model_provider_id.to_string()
        } else {
            metadata.model_provider
        },
        model: metadata.model,
        reasoning_effort: metadata.reasoning_effort,
        created_at: metadata.created_at,
        updated_at: metadata.updated_at,
        recency_at: metadata.recency_at,
        archived_at: metadata.archived_at,
        section: metadata.section,
        section_position: metadata.section_position,
        section_entered_at: metadata.section_entered_at,
        project_id: metadata.project_id,
        daybreak_enabled: metadata.daybreak_enabled,
        cwd: metadata.cwd,
        cli_version: metadata.cli_version,
        originator: metadata.originator,
        source: parse_or_default(&metadata.source, SessionSource::Unknown),
        history_mode,
        thread_source: metadata.thread_source,
        agent_nickname: metadata.agent_nickname,
        agent_role: metadata.agent_role,
        agent_path: metadata.agent_path,
        git_info: git_info_from_parts(
            metadata.git_sha,
            metadata.git_branch,
            metadata.git_origin_url,
        ),
        approval_mode: parse_or_default(&metadata.approval_mode, AskForApproval::OnRequest),
        permission_profile,
        token_usage: None,
        first_user_message: metadata.first_user_message,
        history: None,
    }
}

/// Paginated threads show their explicit name. Legacy threads fall back to a title that adds
/// information beyond the first message.
fn thread_name(metadata: &ThreadMetadata) -> Option<String> {
    let explicit = metadata
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    match metadata.history_mode {
        ThreadHistoryMode::Paginated => explicit,
        ThreadHistoryMode::Legacy => explicit.or_else(|| {
            let title = metadata.title.trim();
            if title.is_empty()
                || metadata.first_user_message.as_deref().map(str::trim) == Some(title)
            {
                None
            } else {
                Some(title.to_string())
            }
        }),
    }
}

/// Apply a metadata patch to the catalog record. Fields the patch leaves unset keep their value.
pub(crate) fn apply_patch(metadata: &mut ThreadMetadata, patch: ThreadMetadataPatch) {
    if let Some(preview) = patch.preview {
        metadata.preview = Some(preview);
    }
    if let Some(title) = patch.title {
        metadata.title = title;
    }
    if let Some(name) = patch.name {
        metadata.name = name;
    }
    if let Some(model_provider) = patch.model_provider {
        metadata.model_provider = model_provider;
    }
    if let Some(model) = patch.model {
        metadata.model = Some(model);
    }
    if let Some(reasoning_effort) = patch.reasoning_effort {
        metadata.reasoning_effort = reasoning_effort;
    }
    if let Some(created_at) = patch.created_at {
        metadata.created_at = created_at;
    }
    if let Some(updated_at) = patch.updated_at {
        metadata.updated_at = updated_at;
    }
    if let Some(source) = patch.source {
        metadata.source = enum_to_string(&source);
    }
    metadata.originator = metadata.originator.take().or(patch.originator);
    metadata.creator_user_id = metadata.creator_user_id.take().or(patch.creator_user_id);
    metadata.creator_account_id = metadata
        .creator_account_id
        .take()
        .or(patch.creator_account_id);
    if let Some(thread_source) = patch.thread_source {
        metadata.thread_source = thread_source;
    }
    if let Some(agent_nickname) = patch.agent_nickname {
        metadata.agent_nickname = agent_nickname;
    }
    if let Some(agent_role) = patch.agent_role {
        metadata.agent_role = agent_role;
    }
    if let Some(agent_path) = patch.agent_path {
        metadata.agent_path = agent_path;
    }
    if let Some(cwd) = patch.cwd {
        metadata.cwd = normalize_cwd(cwd);
    }
    if let Some(cli_version) = patch.cli_version {
        metadata.cli_version = cli_version;
    }
    if let Some(approval_mode) = patch.approval_mode {
        metadata.approval_mode = enum_to_string(&approval_mode);
    }
    if let Some(permission_profile) = patch.permission_profile {
        metadata.sandbox_policy = permission_profile_to_metadata_value(&permission_profile);
    }
    if let Some(token_usage) = patch.token_usage {
        metadata.tokens_used = token_usage.total_tokens.max(0);
    }
    if let Some(first_user_message) = patch.first_user_message {
        metadata.first_user_message = Some(first_user_message);
    }
    if let Some(git_info) = patch.git_info {
        let existing = git_info_from_parts(
            metadata.git_sha.clone(),
            metadata.git_branch.clone(),
            metadata.git_origin_url.clone(),
        );
        let (sha, branch, origin_url) = resolve_git_info_patch(existing, git_info);
        metadata.git_sha = sha;
        metadata.git_branch = branch;
        metadata.git_origin_url = origin_url;
    }
    if let Some(daybreak_enabled) = patch.daybreak_enabled {
        metadata.daybreak_enabled = Some(daybreak_enabled);
    }
}

fn resolve_git_info_patch(
    existing: Option<GitInfo>,
    git_info: GitInfoPatch,
) -> (Option<String>, Option<String>, Option<SanitizedGitUrl>) {
    let (existing_sha, existing_branch, existing_origin_url) = match existing {
        Some(info) => (
            info.commit_hash.map(|sha| sha.0),
            info.branch,
            info.repository_url,
        ),
        None => (None, None, None),
    };
    (
        git_info.sha.unwrap_or(existing_sha),
        git_info.branch.unwrap_or(existing_branch),
        git_info.origin_url.unwrap_or(existing_origin_url),
    )
}

pub(crate) fn git_info_from_parts(
    sha: Option<String>,
    branch: Option<String>,
    origin_url: Option<SanitizedGitUrl>,
) -> Option<GitInfo> {
    if sha.is_none() && branch.is_none() && origin_url.is_none() {
        return None;
    }
    Some(GitInfo {
        commit_hash: sha.as_deref().map(GitSha::new),
        branch,
        repository_url: origin_url,
    })
}

pub(crate) fn enum_to_string<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(value)) => value,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

pub(crate) fn parse_or_default<T: DeserializeOwned>(value: &str, default: T) -> T {
    serde_json::from_str(value)
        .or_else(|_| serde_json::from_value(serde_json::Value::String(value.to_string())))
        .unwrap_or(default)
}

fn normalize_cwd(cwd: PathBuf) -> PathBuf {
    codex_utils_path::normalize_for_path_comparison(cwd.as_path()).unwrap_or(cwd)
}

fn permission_profile_from_metadata_value(value: &str, cwd: &Path) -> PermissionProfile {
    serde_json::from_str::<PermissionProfile>(value)
        .or_else(|_| {
            parse_legacy_sandbox_policy(value)
                .map(|policy| PermissionProfile::from_legacy_sandbox_policy_for_cwd(&policy, cwd))
        })
        .unwrap_or_else(|_| PermissionProfile::read_only())
}

pub(crate) fn permission_profile_to_metadata_value(
    permission_profile: &PermissionProfile,
) -> String {
    serde_json::to_string(permission_profile).unwrap_or_default()
}

fn parse_legacy_sandbox_policy(value: &str) -> serde_json::Result<SandboxPolicy> {
    serde_json::from_str(value)
        .or_else(|_| serde_json::from_value(serde_json::Value::String(value.to_string())))
        .or_else(|_| match value {
            "danger-full-access" => Ok(SandboxPolicy::DangerFullAccess),
            "read-only" => Ok(SandboxPolicy::new_read_only_policy()),
            "workspace-write" => Ok(SandboxPolicy::new_workspace_write_policy()),
            "external-sandbox" => Ok(SandboxPolicy::ExternalSandbox {
                network_access: NetworkAccess::Restricted,
            }),
            _ => serde_json::from_value(serde_json::Value::String(value.to_string())),
        })
}
