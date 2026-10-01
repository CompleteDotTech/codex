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
    ];

    pub fn name(self) -> &'static str {
        match self {
            Domain::Sections => "sections",
            Domain::Projects => "projects",
            Domain::ProjectKeys => "project_keys",
            Domain::Threads => "threads",
            Domain::Attachments => "attachments",
            Domain::SpawnEdges => "spawn_edges",
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
