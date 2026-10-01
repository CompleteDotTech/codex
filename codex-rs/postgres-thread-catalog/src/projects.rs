//! Projects, their roots and idempotent creation, with member recency computed on read.
//!
//! Project writes lock the projects table in share row exclusive mode, which serializes them
//! the way SQLite immediate transactions do. Ids order by byte value to match SQLite.

use crate::catalog::PostgresThreadCatalog;
use anyhow::Result;
use anyhow::anyhow;
use codex_state::CreatedProject;
use codex_state::Project;
use codex_state::ProjectRoot;
use codex_state::ProjectSortKey;
use codex_state::ProjectsPage;
use codex_state::SortDirection;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::postgres::PgRow;
use std::collections::BTreeMap;
use uuid::Uuid;

const PROJECT_SELECT: &str = "SELECT projects.*, \
    (SELECT MAX(recency_at_ms) FROM threads \
     WHERE project_id = projects.id AND archived_at_s IS NULL) AS recency_at_ms \
    FROM projects";

const LOCK_PROJECTS: &str = "LOCK TABLE projects IN SHARE ROW EXCLUSIVE MODE";

impl PostgresThreadCatalog {
    pub async fn set_thread_project(
        &self,
        thread_id: &str,
        project_id: Option<&str>,
    ) -> Result<Option<Option<String>>> {
        let (thread_id, project_id) = (thread_id.to_string(), project_id.map(str::to_string));
        self.write(move |connection| {
            Box::pin(async move {
                lock_projects(connection).await?;
                if let Some(project_id) = &project_id {
                    let exists: bool =
                        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                            .bind(project_id)
                            .fetch_one(&mut *connection)
                            .await?;
                    if !exists {
                        anyhow::bail!("project not found: {project_id}");
                    }
                }
                if Uuid::parse_str(&thread_id).is_err() {
                    return Ok(None);
                }
                let Some(previous) = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT project_id FROM threads WHERE id = $1::uuid",
                )
                .bind(&thread_id)
                .fetch_optional(&mut *connection)
                .await?
                else {
                    return Ok(None);
                };
                if previous != project_id {
                    sqlx::query("UPDATE threads SET project_id = $1 WHERE id = $2::uuid")
                        .bind(&project_id)
                        .bind(&thread_id)
                        .execute(&mut *connection)
                        .await?;
                }
                Ok(Some(previous))
            })
        })
        .await
    }

    pub async fn list_projects(
        &self,
        cursor: Option<&str>,
        limit: usize,
        sort_key: ProjectSortKey,
        sort_direction: SortDirection,
    ) -> Result<ProjectsPage> {
        let query = project_list_query(cursor, limit, sort_key, sort_direction)?;
        let rows = self.fetch_rows(query).await?;
        let mut projects: Vec<Project> = Vec::new();
        for row in rows {
            let id: String = row.try_get("id")?;
            if projects.last().is_none_or(|project| project.id != id) {
                projects.push(project_from_row(&row, Vec::new())?);
            }
            if let Some(path) = row.try_get::<Option<String>, _>("root_path")? {
                let project = projects
                    .last_mut()
                    .ok_or_else(|| anyhow!("project missing for root"))?;
                project.roots.push(ProjectRoot { path });
            }
        }
        let next_cursor = (projects.len() > limit)
            .then(|| project_cursor(&projects[limit - 1], sort_key, sort_direction));
        projects.truncate(limit);
        Ok(ProjectsPage {
            projects,
            next_cursor,
        })
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let id = id.to_string();
        self.read(move |connection| Box::pin(async move { load_project(connection, &id).await }))
            .await
    }

    pub async fn get_project_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<Project>> {
        let key = idempotency_key.to_string();
        self.read(move |connection| {
            Box::pin(async move {
                let project_id = sqlx::query_scalar::<_, String>(
                    "SELECT project_id FROM project_idempotency_keys WHERE key = $1",
                )
                .bind(&key)
                .fetch_optional(&mut *connection)
                .await?;
                let Some(project_id) = project_id else {
                    return Ok(None);
                };
                match load_project(connection, &project_id).await? {
                    Some(project) => Ok(Some(project)),
                    None => anyhow::bail!("idempotency key refers to deleted project: {key}"),
                }
            })
        })
        .await
    }

    pub async fn create_project(
        &self,
        name: String,
        roots: Vec<ProjectRoot>,
        metadata: BTreeMap<String, String>,
        thread_ids: &[String],
        idempotency_key: &str,
    ) -> Result<CreatedProject> {
        let (thread_ids, key) = (thread_ids.to_vec(), idempotency_key.to_string());
        self.write(move |connection| {
            Box::pin(async move {
                lock_projects(connection).await?;
                let existing = sqlx::query_scalar::<_, String>(
                    "SELECT project_id FROM project_idempotency_keys WHERE key = $1",
                )
                .bind(&key)
                .fetch_optional(&mut *connection)
                .await?;
                if let Some(existing) = existing {
                    let Some(project) = load_project(connection, &existing).await? else {
                        anyhow::bail!("idempotency key refers to deleted project: {key}");
                    };
                    return Ok(CreatedProject {
                        project,
                        created: false,
                    });
                }
                for thread_id in &thread_ids {
                    let exists = Uuid::parse_str(thread_id).is_ok()
                        && sqlx::query_scalar::<_, bool>(
                            "SELECT EXISTS(SELECT 1 FROM threads WHERE id = $1::uuid)",
                        )
                        .bind(thread_id)
                        .fetch_one(&mut *connection)
                        .await?;
                    if !exists {
                        anyhow::bail!("thread not found: {thread_id}");
                    }
                }
                let id = Uuid::now_v7().to_string();
                let now = chrono::Utc::now().timestamp_millis();
                let position =
                    sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(position) FROM projects")
                        .fetch_one(&mut *connection)
                        .await?
                        .unwrap_or(-1)
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("project position overflow"))?;
                sqlx::query(
                    "INSERT INTO projects (id, name, metadata, position, \
                     created_at_ms, updated_at_ms) VALUES ($1, $2, $3, $4, $5, $5)",
                )
                .bind(&id)
                .bind(&name)
                .bind(serde_json::to_string(&metadata)?)
                .bind(position)
                .bind(now)
                .execute(&mut *connection)
                .await?;
                replace_roots(connection, &id, &roots).await?;
                for thread_id in &thread_ids {
                    sqlx::query("UPDATE threads SET project_id = $1 WHERE id = $2::uuid")
                        .bind(&id)
                        .bind(thread_id)
                        .execute(&mut *connection)
                        .await?;
                }
                sqlx::query(
                    "INSERT INTO project_idempotency_keys (key, project_id, \
                     created_at_ms) VALUES ($1, $2, $3)",
                )
                .bind(&key)
                .bind(&id)
                .bind(now)
                .execute(&mut *connection)
                .await?;
                let row = QueryBuilder::<Postgres>::new(PROJECT_SELECT)
                    .push(" WHERE id = ")
                    .push_bind(&id)
                    .build()
                    .fetch_one(&mut *connection)
                    .await?;
                Ok(CreatedProject {
                    project: project_from_row(&row, roots)?,
                    created: true,
                })
            })
        })
        .await
    }

    pub async fn update_project(
        &self,
        id: &str,
        name: Option<String>,
        roots: Option<Vec<ProjectRoot>>,
        metadata: Option<BTreeMap<String, String>>,
    ) -> Result<Option<(Project, bool)>> {
        let id = id.to_string();
        self.write(move |connection| {
            Box::pin(async move {
                lock_projects(connection).await?;
                let Some(current) = load_project(connection, &id).await? else {
                    return Ok(None);
                };
                let next_name = name.unwrap_or_else(|| current.name.clone());
                let next_roots = roots.unwrap_or_else(|| current.roots.clone());
                let next_metadata = metadata.unwrap_or_else(|| current.metadata.clone());
                if next_name == current.name
                    && next_roots == current.roots
                    && next_metadata == current.metadata
                {
                    return Ok(Some((current, false)));
                }
                let now = chrono::Utc::now().timestamp_millis();
                sqlx::query(
                    "UPDATE projects SET name = $1, metadata = $2, \
                     updated_at_ms = $3 WHERE id = $4",
                )
                .bind(&next_name)
                .bind(serde_json::to_string(&next_metadata)?)
                .bind(now)
                .bind(&id)
                .execute(&mut *connection)
                .await?;
                if next_roots != current.roots {
                    replace_roots(connection, &id, &next_roots).await?;
                }
                Ok(Some((
                    Project {
                        id,
                        name: next_name,
                        roots: next_roots,
                        metadata: next_metadata,
                        position: current.position,
                        created_at_ms: current.created_at_ms,
                        updated_at_ms: now,
                        recency_at_ms: current.recency_at_ms,
                    },
                    true,
                )))
            })
        })
        .await
    }

    /// Move a project before another project, or append it when the anchor is absent.
    ///
    /// Returns None when the moved project does not exist and Some(false) for a no-op.
    pub async fn move_project(
        &self,
        project_id: &str,
        before_project_id: Option<&str>,
    ) -> Result<Option<bool>> {
        let (project_id, before_project_id) = (
            project_id.to_string(),
            before_project_id.map(str::to_string),
        );
        self.write(move |connection| {
            Box::pin(async move {
                lock_projects(connection).await?;
                let mut project_ids = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM projects \
                     ORDER BY position ASC, id COLLATE \"C\" ASC",
                )
                .fetch_all(&mut *connection)
                .await?;
                let Some(current_index) = project_ids.iter().position(|id| *id == project_id)
                else {
                    return Ok(None);
                };
                if before_project_id.as_deref() == Some(project_id.as_str()) {
                    anyhow::bail!("project {project_id} cannot be moved before itself");
                }
                let original_project_ids = project_ids.clone();
                project_ids.remove(current_index);
                let next_index = if let Some(before) = &before_project_id {
                    project_ids
                        .iter()
                        .position(|id| id == before)
                        .ok_or_else(|| anyhow!("before project not found: {before}"))?
                } else {
                    project_ids.len()
                };
                project_ids.insert(next_index, project_id.clone());
                if project_ids == original_project_ids {
                    return Ok(Some(false));
                }
                for (position, id) in project_ids.iter().enumerate() {
                    sqlx::query("UPDATE projects SET position = $1 WHERE id = $2")
                        .bind(position as i64)
                        .bind(id)
                        .execute(&mut *connection)
                        .await?;
                }
                sqlx::query("UPDATE projects SET updated_at_ms = $1 WHERE id = $2")
                    .bind(chrono::Utc::now().timestamp_millis())
                    .bind(&project_id)
                    .execute(&mut *connection)
                    .await?;
                Ok(Some(true))
            })
        })
        .await
    }

    pub async fn delete_project(&self, id: &str) -> Result<Option<(Vec<String>, Vec<String>)>> {
        let id = id.to_string();
        self.write(move |connection| {
            Box::pin(async move {
                lock_projects(connection).await?;
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                        .bind(&id)
                        .fetch_one(&mut *connection)
                        .await?;
                if !exists {
                    return Ok(None);
                }
                let mut member_ids = Vec::new();
                for archived in [false, true] {
                    member_ids.push(
                        sqlx::query_scalar::<_, String>(
                            "SELECT id::text FROM threads \
                             WHERE project_id = $1 AND (archived_at_s IS NOT NULL) = $2 \
                             ORDER BY id ASC",
                        )
                        .bind(&id)
                        .bind(archived)
                        .fetch_all(&mut *connection)
                        .await?,
                    );
                }
                sqlx::query("UPDATE threads SET project_id = NULL WHERE project_id = $1")
                    .bind(&id)
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("DELETE FROM projects WHERE id = $1")
                    .bind(&id)
                    .execute(&mut *connection)
                    .await?;
                let archived = member_ids.pop().unwrap_or_default();
                let active = member_ids.pop().unwrap_or_default();
                Ok(Some((active, archived)))
            })
        })
        .await
    }
}

async fn lock_projects(connection: &mut PgConnection) -> Result<()> {
    sqlx::query(LOCK_PROJECTS).execute(connection).await?;
    Ok(())
}

async fn load_project(connection: &mut PgConnection, id: &str) -> Result<Option<Project>> {
    let row = QueryBuilder::<Postgres>::new(PROJECT_SELECT)
        .push(" WHERE id = ")
        .push_bind(id.to_string())
        .build()
        .fetch_optional(&mut *connection)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let roots =
        sqlx::query("SELECT path FROM project_roots WHERE project_id = $1 ORDER BY position ASC")
            .bind(id)
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|row| {
                Ok(ProjectRoot {
                    path: row.try_get("path")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
    project_from_row(&row, roots).map(Some)
}

fn project_from_row(row: &PgRow, roots: Vec<ProjectRoot>) -> Result<Project> {
    Ok(Project {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        roots,
        metadata: serde_json::from_str(&row.try_get::<String, _>("metadata")?)?,
        position: row.try_get("position")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        recency_at_ms: row.try_get("recency_at_ms")?,
    })
}

async fn replace_roots(
    connection: &mut PgConnection,
    project_id: &str,
    roots: &[ProjectRoot],
) -> Result<()> {
    sqlx::query("DELETE FROM project_roots WHERE project_id = $1")
        .bind(project_id)
        .execute(&mut *connection)
        .await?;
    for (position, root) in roots.iter().enumerate() {
        sqlx::query(
            "INSERT INTO project_roots (project_id, position, path) \
             VALUES ($1, $2, $3)",
        )
        .bind(project_id)
        .bind(position as i64)
        .bind(&root.path)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

fn project_list_query(
    cursor: Option<&str>,
    limit: usize,
    sort_key: ProjectSortKey,
    sort_direction: SortDirection,
) -> Result<QueryBuilder<Postgres>> {
    anyhow::ensure!(limit > 0, "project limit must be positive");
    let query_limit = i64::try_from(limit)?
        .checked_add(/*rhs*/ 1)
        .ok_or_else(|| anyhow!("project limit overflow"))?;
    let column = match sort_key {
        ProjectSortKey::Position => "p.position",
        ProjectSortKey::RecencyAt => "p.recency_at_ms",
    };
    let operator = match sort_direction {
        SortDirection::Asc => ">",
        SortDirection::Desc => "<",
    };
    let mut query = QueryBuilder::new(format!(
        "WITH project_activity AS ({PROJECT_SELECT}), page AS (SELECT * FROM project_activity p"
    ));
    if let Some(cursor) = cursor {
        let (value, id) = parse_project_cursor(cursor, sort_key, sort_direction)?;
        if let Some(value) = value {
            query
                .push(format!(" WHERE ({column} {operator} "))
                .push_bind(value);
            query.push(format!(" OR ({column} = ")).push_bind(value);
            query
                .push(format!(" AND p.id COLLATE \"C\" {operator} "))
                .push_bind(id)
                .push(" COLLATE \"C\")");
            if sort_key == ProjectSortKey::RecencyAt {
                query.push(" OR p.recency_at_ms IS NULL");
            }
            query.push(")");
        } else {
            query
                .push(format!(
                    " WHERE p.recency_at_ms IS NULL AND p.id COLLATE \"C\" {operator} "
                ))
                .push_bind(id)
                .push(" COLLATE \"C\"");
        }
    }
    push_project_order(&mut query, sort_key, sort_direction);
    query.push(" LIMIT ").push_bind(query_limit);
    query.push(
        ") SELECT p.*, roots.path AS root_path FROM page p \
        LEFT JOIN project_roots roots ON roots.project_id = p.id",
    );
    push_project_order(&mut query, sort_key, sort_direction);
    query.push(", roots.position ASC");
    Ok(query)
}

fn push_project_order(
    query: &mut QueryBuilder<Postgres>,
    sort_key: ProjectSortKey,
    sort_direction: SortDirection,
) {
    let direction = match sort_direction {
        SortDirection::Asc => "ASC",
        SortDirection::Desc => "DESC",
    };
    query.push(" ORDER BY ");
    match sort_key {
        ProjectSortKey::Position => query.push("p.position"),
        ProjectSortKey::RecencyAt => query.push("p.recency_at_ms IS NULL ASC, p.recency_at_ms"),
    };
    query.push(format!(" {direction}, p.id COLLATE \"C\" {direction}"));
}

fn project_cursor(project: &Project, sort_key: ProjectSortKey, direction: SortDirection) -> String {
    if sort_key == ProjectSortKey::Position && direction == SortDirection::Asc {
        // Retain the existing format for clients reconnecting to an older server.
        return format!("{}|{}", project.position, project.id);
    }
    let (key, value) = match sort_key {
        ProjectSortKey::Position => ("position", project.position.to_string()),
        ProjectSortKey::RecencyAt => (
            "recencyAt",
            project
                .recency_at_ms
                .map_or_else(|| "null".to_string(), |value| value.to_string()),
        ),
    };
    let direction = match direction {
        SortDirection::Asc => "asc",
        SortDirection::Desc => "desc",
    };
    format!("v1|{key}|{direction}|{value}|{}", project.id)
}

fn parse_project_cursor(
    cursor: &str,
    sort_key: ProjectSortKey,
    direction: SortDirection,
) -> Result<(Option<i64>, String)> {
    let invalid = || anyhow!("invalid project cursor: malformed or mismatched sort anchor");
    if cursor.len() > 128 {
        return Err(invalid());
    }
    let parts: Vec<_> = cursor.split('|').collect();
    let key = match sort_key {
        ProjectSortKey::Position => "position",
        ProjectSortKey::RecencyAt => "recencyAt",
    };
    let order = match direction {
        SortDirection::Asc => "asc",
        SortDirection::Desc => "desc",
    };
    let (value, id) = match parts.as_slice() {
        [value, id] if sort_key == ProjectSortKey::Position && direction == SortDirection::Asc => {
            (*value, *id)
        }
        ["v1", cursor_key, cursor_order, value, id]
            if *cursor_key == key && *cursor_order == order =>
        {
            (*value, *id)
        }
        _ => return Err(invalid()),
    };
    let value = if value == "null" && sort_key == ProjectSortKey::RecencyAt {
        None
    } else {
        let parsed: i64 = value.parse().map_err(|_| invalid())?;
        if parsed.to_string() != value || sort_key == ProjectSortKey::Position && parsed < 0 {
            return Err(invalid());
        }
        Some(parsed)
    };
    let uuid = Uuid::parse_str(id).map_err(|_| invalid())?;
    if uuid.to_string() != id {
        return Err(invalid());
    }
    Ok((value, id.to_string()))
}
