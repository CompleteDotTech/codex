//! The independent parts of a store, in the order they must be written.

use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use std::future::Future;

use crate::source::SqliteSource;

/// One independently verified part of a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Domain {
    Sections,
    Projects,
    ProjectKeys,
    Threads,
    Attachments,
    SpawnEdges,
    Goals,
    QueuedItems,
    QueueRevisions,
    Logs,
    MemoryOutputs,
    MemoryJobs,
    MemoryProgress,
    BoardDeleted,
    BoardChannels,
    BoardPosts,
    BoardSubscriptions,
    BoardOptOuts,
    ExternalImports,
}

impl Domain {
    /// Every domain, ordered so that a record's references are written before the record.
    pub const ALL: &'static [Domain] = &[
        Domain::Sections,
        Domain::Projects,
        Domain::ProjectKeys,
        Domain::Threads,
        Domain::Attachments,
        Domain::SpawnEdges,
        Domain::Goals,
        Domain::QueuedItems,
        Domain::QueueRevisions,
        Domain::Logs,
        Domain::MemoryOutputs,
        Domain::MemoryJobs,
        Domain::MemoryProgress,
        Domain::BoardDeleted,
        Domain::BoardChannels,
        Domain::BoardPosts,
        Domain::BoardSubscriptions,
        Domain::BoardOptOuts,
        Domain::ExternalImports,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Domain::Sections => "sections",
            Domain::Projects => "projects",
            Domain::ProjectKeys => "project_keys",
            Domain::Threads => "threads",
            Domain::Attachments => "attachments",
            Domain::SpawnEdges => "spawn_edges",
            Domain::Goals => "goals",
            Domain::QueuedItems => "queued_items",
            Domain::QueueRevisions => "queue_revisions",
            Domain::Logs => "logs",
            Domain::MemoryOutputs => "memory_outputs",
            Domain::MemoryJobs => "memory_jobs",
            Domain::MemoryProgress => "memory_progress",
            Domain::BoardDeleted => "board_deleted",
            Domain::BoardChannels => "board_channels",
            Domain::BoardPosts => "board_posts",
            Domain::BoardSubscriptions => "board_subscriptions",
            Domain::BoardOptOuts => "board_opt_outs",
            Domain::ExternalImports => "external_imports",
        }
    }
}

/// How one domain moves: read a page from the source, write a page to the target inside the
/// caller's transaction, and read the target back so it can be compared.
pub(crate) trait DomainOps {
    type Record: Serialize + Send + Sync;

    const DOMAIN: Domain;

    /// The record's position in the domain's total order; pages resume after a key.
    fn key(record: &Self::Record) -> String;

    fn export<'a>(
        source: &'a SqliteSource,
        after: Option<&'a str>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Self::Record>>> + Send + 'a;

    fn import<'a>(
        connection: &'a mut PgConnection,
        records: &'a [Self::Record],
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn read_back<'a>(
        connection: &'a mut PgConnection,
        after: Option<&'a str>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Self::Record>>> + Send + 'a;
}
