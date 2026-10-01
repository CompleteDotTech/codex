//! Thread listing, search and pagination with the SQLite ordering contract.
//!
//! SQLite sorts NULL before every value, so ascending order places NULLs first and descending
//! order places them last. PostgreSQL does the opposite by default, so every ordered column
//! spells out its NULL placement.

use crate::catalog::PostgresThreadCatalog;
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use codex_postgres_thread_rows::thread_columns;
use codex_postgres_thread_rows::thread_metadata_from_row;
use codex_protocol::ThreadId;
use codex_state::Anchor;
use codex_state::SortDirection;
use codex_state::SortKey;
use codex_state::ThreadFilterOptions;
use codex_state::ThreadMetadata;
use codex_state::ThreadRelationFilter;
use codex_state::ThreadsPage;
use sqlx::Postgres;
use sqlx::QueryBuilder;
use sqlx::Row;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::time::timeout;

const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

impl PostgresThreadCatalog {
    /// List threads using the underlying database.
    pub async fn list_threads(
        &self,
        page_size: usize,
        filters: ThreadFilterOptions<'_>,
    ) -> Result<ThreadsPage> {
        self.list_threads_matching(page_size, filters, /*relation_filter*/ None)
            .await
    }

    /// List direct children of `parent_thread_id` using persisted spawn edges.
    pub async fn list_threads_by_parent(
        &self,
        page_size: usize,
        parent_thread_id: ThreadId,
        filters: ThreadFilterOptions<'_>,
    ) -> Result<ThreadsPage> {
        self.list_threads_by_relation(
            page_size,
            ThreadRelationFilter::DirectChildrenOf(parent_thread_id),
            filters,
        )
        .await
    }

    /// List threads matching a persisted spawn-graph relationship.
    pub async fn list_threads_by_relation(
        &self,
        page_size: usize,
        relation_filter: ThreadRelationFilter,
        filters: ThreadFilterOptions<'_>,
    ) -> Result<ThreadsPage> {
        self.list_threads_matching(page_size, filters, Some(relation_filter))
            .await
    }

    async fn list_threads_matching(
        &self,
        page_size: usize,
        filters: ThreadFilterOptions<'_>,
        relation_filter: Option<ThreadRelationFilter>,
    ) -> Result<ThreadsPage> {
        let limit = page_size.saturating_add(1);
        let mut builder = QueryBuilder::<Postgres>::new("");
        push_list_threads_query(&mut builder, filters, relation_filter, limit);
        let rows = self.fetch_rows(builder).await?;

        let mut items = Vec::with_capacity(rows.len());
        let mut parent_thread_ids = HashMap::new();
        for row in &rows {
            let item = thread_metadata_from_row(row)?;
            if relation_filter.is_some()
                && let Some(parent_thread_id) =
                    row.try_get::<Option<String>, _>("parent_thread_id")?
            {
                parent_thread_ids.insert(item.id, ThreadId::try_from(parent_thread_id)?);
            }
            items.push(item);
        }
        let num_scanned_rows = items.len();
        let next_anchor = if items.len() > page_size {
            if let Some(overflow_item) = items.pop() {
                parent_thread_ids.remove(&overflow_item.id);
            }
            items.last().and_then(|item| {
                anchor_from_item(item, filters.sort_key, relation_filter.is_some())
            })
        } else {
            None
        };
        Ok(ThreadsPage {
            items,
            parent_thread_ids,
            next_anchor,
            num_scanned_rows,
        })
    }

    /// List thread ids using the underlying database (no rollout scanning).
    pub async fn list_thread_ids(
        &self,
        limit: usize,
        anchor: Option<&Anchor>,
        sort_key: SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> Result<Vec<ThreadId>> {
        let tiebreaker = matches!(sort_key, SortKey::RecencyAt | SortKey::SectionPosition);
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT threads.id::text AS id FROM codex_storage.threads",
        );
        push_thread_filters(
            &mut builder,
            ThreadFilterOptions {
                archived_only,
                allowed_sources,
                model_providers,
                cwd_filters: None,
                section: None,
                project_id: None,
                anchor,
                sort_key,
                sort_direction: SortDirection::Desc,
                search_term: None,
            },
            tiebreaker,
            /*include_empty_preview*/ false,
        );
        push_order_and_limit(
            &mut builder,
            sort_key,
            SortDirection::Desc,
            tiebreaker,
            limit,
        );
        self.fetch_rows(builder)
            .await?
            .iter()
            .map(|row| Ok(ThreadId::try_from(row.try_get::<String, _>("id")?)?))
            .collect()
    }

    /// Find the newest thread whose user-facing title exactly matches `title`.
    pub async fn find_thread_by_exact_title(
        &self,
        title: &str,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
        cwd: Option<&Path>,
    ) -> Result<Option<ThreadMetadata>> {
        let mut builder = QueryBuilder::<Postgres>::new("");
        push_select_columns(&mut builder);
        builder.push(" FROM codex_storage.threads");
        push_thread_filters(
            &mut builder,
            ThreadFilterOptions {
                archived_only,
                allowed_sources,
                model_providers,
                cwd_filters: None,
                section: None,
                project_id: None,
                anchor: None,
                sort_key: SortKey::UpdatedAt,
                sort_direction: SortDirection::Desc,
                search_term: None,
            },
            /*include_thread_id_tiebreaker*/ false,
            /*include_empty_preview*/ false,
        );
        builder
            .push(" AND threads.title = ")
            .push_bind(title.to_string());
        if let Some(cwd) = cwd {
            builder
                .push(" AND threads.origin_cwd = ")
                .push_bind(cwd.display().to_string());
        }
        push_order_and_limit(
            &mut builder,
            SortKey::UpdatedAt,
            SortDirection::Desc,
            /*include_thread_id_tiebreaker*/ false,
            /*limit*/ 1,
        );
        self.fetch_rows(builder)
            .await?
            .first()
            .map(thread_metadata_from_row)
            .transpose()
    }

    async fn fetch_rows(
        &self,
        mut builder: QueryBuilder<Postgres>,
    ) -> Result<Vec<sqlx::postgres::PgRow>> {
        let mut connection = self
            .pool()
            .acquire()
            .await
            .map_err(|error| anyhow!("PostgreSQL thread storage is unavailable: {error:?}"))?;
        timeout(QUERY_TIMEOUT, builder.build().fetch_all(&mut *connection))
            .await
            .map_err(|_| anyhow!("PostgreSQL thread query timed out"))?
            .map_err(Into::into)
    }
}

fn push_select_columns(builder: &mut QueryBuilder<Postgres>) {
    builder.push(concat!("SELECT ", thread_columns!()));
}

fn push_list_threads_query(
    builder: &mut QueryBuilder<Postgres>,
    filters: ThreadFilterOptions<'_>,
    relation_filter: Option<ThreadRelationFilter>,
    limit: usize,
) {
    if let Some(ThreadRelationFilter::DescendantsOf(ancestor_thread_id)) = relation_filter {
        builder.push(
            "WITH RECURSIVE subtree(child_thread_id, parent_thread_id) AS ( \
             SELECT child_thread_id, parent_thread_id \
             FROM codex_storage.thread_spawn_edges WHERE parent_thread_id = ",
        );
        builder.push_bind(ancestor_thread_id.to_string()).push(
            "::uuid UNION SELECT edge.child_thread_id, edge.parent_thread_id \
                 FROM codex_storage.thread_spawn_edges AS edge \
                 JOIN subtree ON edge.parent_thread_id = subtree.child_thread_id) ",
        );
    }
    push_select_columns(builder);
    match relation_filter {
        Some(ThreadRelationFilter::DirectChildrenOf(_)) => builder.push(
            ", listed_edge.parent_thread_id::text AS parent_thread_id \
             FROM codex_storage.thread_spawn_edges AS listed_edge \
             JOIN codex_storage.threads ON threads.id = listed_edge.child_thread_id",
        ),
        Some(ThreadRelationFilter::DescendantsOf(_)) => builder.push(
            ", subtree.parent_thread_id::text AS parent_thread_id \
             FROM subtree JOIN codex_storage.threads ON threads.id = subtree.child_thread_id",
        ),
        None => builder.push(" FROM codex_storage.threads"),
    };
    let include_thread_id_tiebreaker = relation_filter.is_some()
        || matches!(
            filters.sort_key,
            SortKey::RecencyAt | SortKey::SectionPosition
        );
    push_thread_filters(
        builder,
        filters,
        include_thread_id_tiebreaker,
        /*include_empty_preview*/ relation_filter.is_some(),
    );
    match relation_filter {
        Some(ThreadRelationFilter::DirectChildrenOf(parent_thread_id)) => {
            builder
                .push(" AND listed_edge.parent_thread_id = ")
                .push_bind(parent_thread_id.to_string())
                .push("::uuid");
        }
        Some(ThreadRelationFilter::DescendantsOf(ancestor_thread_id)) => {
            builder
                .push(" AND subtree.child_thread_id <> ")
                .push_bind(ancestor_thread_id.to_string())
                .push("::uuid");
        }
        None => {}
    }
    push_order_and_limit(
        builder,
        filters.sort_key,
        filters.sort_direction,
        include_thread_id_tiebreaker,
        limit,
    );
}

fn sort_column(sort_key: SortKey) -> &'static str {
    match sort_key {
        SortKey::CreatedAt => "threads.created_at_ms",
        SortKey::UpdatedAt => "threads.updated_at_ms",
        SortKey::RecencyAt => "threads.recency_at_ms",
        SortKey::SectionPosition => "threads.section_position",
    }
}

fn push_thread_filters(
    builder: &mut QueryBuilder<Postgres>,
    options: ThreadFilterOptions<'_>,
    include_thread_id_tiebreaker: bool,
    include_empty_preview: bool,
) {
    let ThreadFilterOptions {
        archived_only,
        allowed_sources,
        model_providers,
        cwd_filters,
        section,
        project_id,
        anchor,
        sort_key,
        sort_direction,
        search_term,
    } = options;
    builder.push(" WHERE TRUE");
    if archived_only {
        builder.push(" AND threads.archived_at_s IS NOT NULL");
    } else {
        builder.push(" AND threads.archived_at_s IS NULL");
    }
    if !archived_only && !include_empty_preview && !matches!(section, Some(Some(_))) {
        builder.push(" AND COALESCE(threads.preview, '') <> ''");
    }
    match section {
        Some(Some(section)) => {
            builder
                .push(" AND threads.thread_section_id = ")
                .push_bind(section.to_string());
        }
        Some(None) => {
            builder.push(" AND threads.thread_section_id IS NULL");
        }
        None => {}
    }
    match project_id {
        Some(Some(project_id)) => {
            builder
                .push(" AND threads.project_id = ")
                .push_bind(project_id.to_string());
        }
        Some(None) => {
            builder.push(" AND threads.project_id IS NULL");
        }
        None => {}
    }
    if !allowed_sources.is_empty() {
        builder
            .push(" AND threads.source = ANY(")
            .push_bind(allowed_sources.to_vec())
            .push(")");
    }
    if let Some(model_providers) = model_providers
        && !model_providers.is_empty()
    {
        builder
            .push(" AND threads.model_provider = ANY(")
            .push_bind(model_providers.to_vec())
            .push(")");
    }
    match cwd_filters {
        Some([]) => {
            builder.push(" AND FALSE");
        }
        Some(cwd_filters) => {
            builder
                .push(" AND threads.origin_cwd = ANY(")
                .push_bind(
                    cwd_filters
                        .iter()
                        .map(|cwd| cwd.display().to_string())
                        .collect::<Vec<_>>(),
                )
                .push(")");
        }
        None => {}
    }
    if let Some(search_term) = search_term {
        builder
            .push(" AND (strpos(COALESCE(threads.name, ''), ")
            .push_bind(search_term.to_string())
            .push(") > 0 OR strpos(threads.title, ")
            .push_bind(search_term.to_string())
            .push(") > 0 OR strpos(COALESCE(threads.preview, ''), ")
            .push_bind(search_term.to_string())
            .push(") > 0)");
    }
    if let Some(anchor) = anchor {
        let anchor_ts = anchor.ts.timestamp_millis();
        let column = sort_column(sort_key);
        let operator = match sort_direction {
            SortDirection::Asc => ">",
            SortDirection::Desc => "<",
        };
        builder
            .push(" AND (")
            .push(column)
            .push(" ")
            .push(operator)
            .push(" ")
            .push_bind(anchor_ts);
        if include_thread_id_tiebreaker && let Some(anchor_id) = anchor.id {
            builder
                .push(" OR (")
                .push(column)
                .push(" = ")
                .push_bind(anchor_ts)
                .push(" AND threads.id ")
                .push(operator)
                .push(" ")
                .push_bind(anchor_id.to_string())
                .push("::uuid)");
        }
        builder.push(")");
    }
}

fn push_order_and_limit(
    builder: &mut QueryBuilder<Postgres>,
    sort_key: SortKey,
    sort_direction: SortDirection,
    include_thread_id_tiebreaker: bool,
    limit: usize,
) {
    let (order_direction, nulls) = match sort_direction {
        SortDirection::Asc => ("ASC", "NULLS FIRST"),
        SortDirection::Desc => ("DESC", "NULLS LAST"),
    };
    builder
        .push(" ORDER BY ")
        .push(sort_column(sort_key))
        .push(" ")
        .push(order_direction)
        .push(" ")
        .push(nulls);
    if include_thread_id_tiebreaker {
        builder.push(", threads.id ").push(order_direction);
    }
    builder
        .push(" LIMIT ")
        .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
}

/// The cursor for the last returned item, mirroring the SQLite store.
fn anchor_from_item(
    item: &ThreadMetadata,
    sort_key: SortKey,
    include_thread_id_tiebreaker: bool,
) -> Option<Anchor> {
    let ts = match sort_key {
        SortKey::CreatedAt => item.created_at,
        SortKey::UpdatedAt => item.updated_at,
        SortKey::RecencyAt => item.recency_at,
        SortKey::SectionPosition => DateTime::from_timestamp_millis(item.section_position?)?,
    };
    Some(Anchor {
        ts,
        id: (include_thread_id_tiebreaker
            || matches!(sort_key, SortKey::RecencyAt | SortKey::SectionPosition))
        .then_some(item.id),
    })
}
