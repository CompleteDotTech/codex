use crate::RemoteStorageError;
use codex_agent_graph_store::PostgresAgentGraphStore;
use codex_agent_message_board_extension::MessageBoardHost;
use codex_postgres_goal_store::PostgresGoalStore;
use codex_postgres_import_store::PostgresExternalAgentImportStore;
use codex_postgres_log_store::PostgresLogStore;
use codex_postgres_memory_store::PostgresMemoryStore;
use codex_postgres_message_board::PostgresAgentMessageBoard;
use codex_postgres_queue_store::PostgresQueueStore;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::NamedNamespace;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::ThreadOwnershipNamespace;
use codex_postgres_runtime::check_runtime_schema;
use codex_postgres_thread_catalog::PostgresThreadCatalog;
use codex_postgres_thread_store::PostgresThreadStore;
use codex_protocol::SessionId;
use codex_storage_authority::HostCredentialResolver;
use codex_storage_authority::RemotePostgresProfile;
use sqlx::Row;
use std::sync::Arc;
use std::time::Duration;

/// The schema every client without an explicit named namespace uses.
const DEFAULT_SCHEMA: &str = "codex_storage";
const DEFAULT_RUNTIME_LOGIN: &str = "codex_runtime";

/// Whether the dataset accepts writes, and which activation it is at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageActivation {
    /// True while a migration holds the dataset and every store refuses writes.
    pub migrating: bool,
    /// True once the dataset was handed back to local storage; it never accepts writes again.
    pub retired: bool,
    /// Increases each time a verified migration is activated.
    pub generation: i64,
    /// The dataset the last activation published; `None` before the first one.
    pub dataset_id: Option<uuid::Uuid>,
}

/// A verified connection to one remote dataset.
#[derive(Clone)]
pub struct RemoteStorage {
    pool: Arc<PostgresPool>,
    namespace: Option<NamedNamespace>,
    connected_generation: i64,
}

impl std::fmt::Debug for RemoteStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteStorage([redacted])")
    }
}

impl RemoteStorage {
    /// Connect with the runtime login and verify the dataset without changing it.
    pub async fn connect(
        profile: &RemotePostgresProfile,
        resolver: &HostCredentialResolver<'_>,
    ) -> Result<Self, RemoteStorageError> {
        let namespace = if profile.namespace() == DEFAULT_SCHEMA {
            None
        } else {
            Some(
                NamedNamespace::new(profile.namespace())
                    .map_err(|_| RemoteStorageError::UnsupportedNamespace)?,
            )
        };
        let ca_certificate = profile
            .ca_certificate()
            .ok_or(RemoteStorageError::CaCertificateRequired)?;
        let credential = resolver
            .resolve(profile.credential())
            .map_err(RemoteStorageError::Credential)?;
        let username = namespace
            .as_ref()
            .map_or(DEFAULT_RUNTIME_LOGIN, NamedNamespace::runtime_login);
        let settings = ConnectionSettings {
            host: profile.endpoint().to_owned(),
            port: profile.port(),
            database: profile.database().to_owned(),
            username: username.to_owned(),
            password: credential.into_zeroizing().into(),
            ca_certificate: ca_certificate.to_path_buf(),
            limits: PoolLimits {
                connect_timeout: Duration::from_secs(u64::from(profile.connect_timeout_seconds())),
                acquire_timeout: Duration::from_secs(u64::from(
                    profile.pool_acquire_timeout_seconds(),
                )),
                max_connections: u32::from(profile.max_connections()),
            },
        };
        let pool = Arc::new(
            PostgresPool::connect_in_namespace(settings, namespace.as_ref())
                .await
                .map_err(RemoteStorageError::Connection)?,
        );
        check_runtime_schema(&pool)
            .await
            .map_err(RemoteStorageError::Schema)?;
        let mut storage = Self {
            pool,
            namespace,
            connected_generation: 0,
        };
        let activation = storage.activation().await?;
        storage.connected_generation = activation.generation;
        Ok(storage)
    }

    /// The activation state as the dataset reports it right now.
    pub async fn activation(&self) -> Result<StorageActivation, RemoteStorageError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(RemoteStorageError::Connection)?;
        let row = sqlx::query(
            "SELECT state, generation, dataset_id FROM storage_activation WHERE singleton",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| {
            RemoteStorageError::Schema(codex_postgres_runtime::RuntimeSchemaError::Unavailable)
        })?;
        let state: String = row.try_get("state").map_err(|_| {
            RemoteStorageError::Schema(codex_postgres_runtime::RuntimeSchemaError::Invalid)
        })?;
        let generation: i64 = row.try_get("generation").map_err(|_| {
            RemoteStorageError::Schema(codex_postgres_runtime::RuntimeSchemaError::Invalid)
        })?;
        let dataset: Option<String> = row.try_get("dataset_id").map_err(|_| {
            RemoteStorageError::Schema(codex_postgres_runtime::RuntimeSchemaError::Invalid)
        })?;
        Ok(StorageActivation {
            migrating: state == "migrating",
            retired: state == "retired",
            generation,
            dataset_id: dataset.and_then(|value| uuid::Uuid::parse_str(&value).ok()),
        })
    }

    /// The generation this connection was opened against.
    pub fn connected_generation(&self) -> i64 {
        self.connected_generation
    }

    /// Fail unless the dataset is open and still at the generation this connection saw. A host
    /// calls this before it resumes work after a reconnect, so a client never keeps writing a
    /// dataset that was re-activated underneath it.
    pub async fn require_current_generation(&self) -> Result<(), RemoteStorageError> {
        let activation = self.activation().await?;
        if activation.retired {
            return Err(RemoteStorageError::Retired);
        }
        if activation.migrating {
            return Err(RemoteStorageError::Migrating);
        }
        if activation.generation != self.connected_generation {
            return Err(RemoteStorageError::GenerationChanged);
        }
        Ok(())
    }

    fn ownership_namespace(&self) -> ThreadOwnershipNamespace {
        self.namespace.clone().map_or(
            ThreadOwnershipNamespace::Default,
            ThreadOwnershipNamespace::Named,
        )
    }

    pub fn thread_catalog(&self) -> PostgresThreadCatalog {
        PostgresThreadCatalog::new(self.pool.clone())
    }

    pub fn thread_store(
        &self,
        default_model_provider_id: impl Into<String>,
    ) -> Arc<PostgresThreadStore> {
        Arc::new(
            PostgresThreadStore::new(self.pool.clone(), default_model_provider_id)
                .with_namespace(self.ownership_namespace()),
        )
    }

    pub fn queue_store(&self) -> Arc<PostgresQueueStore> {
        Arc::new(PostgresQueueStore::new(self.pool.clone()))
    }

    pub fn goal_store(&self) -> Arc<PostgresGoalStore> {
        Arc::new(PostgresGoalStore::new(self.pool.clone()))
    }

    pub fn log_store(&self) -> Arc<PostgresLogStore> {
        Arc::new(PostgresLogStore::new(self.pool.clone()))
    }

    pub fn memory_store(&self) -> Arc<PostgresMemoryStore> {
        Arc::new(PostgresMemoryStore::new(self.pool.clone()))
    }

    pub fn agent_graph_store(&self) -> Arc<PostgresAgentGraphStore> {
        Arc::new(PostgresAgentGraphStore::new(self.pool.clone()))
    }

    pub fn external_import_store(&self) -> Arc<PostgresExternalAgentImportStore> {
        Arc::new(PostgresExternalAgentImportStore::new(self.pool.clone()))
    }

    pub fn message_board(
        &self,
        identity: SessionId,
        host: Arc<dyn MessageBoardHost>,
    ) -> PostgresAgentMessageBoard {
        PostgresAgentMessageBoard::new(self.pool.clone(), identity, host)
    }

    /// Close every connection. Stores built from this handle fail afterwards.
    pub async fn close(&self) {
        let _ = self.pool.close().await;
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
