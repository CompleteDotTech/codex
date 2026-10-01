//! Creates isolated v2 memory state on demand while sharing the source thread catalog.
//! Reset and thread deletion always cover every existing version.

use super::MemoryStore;
use super::MemoryStoreSelection;
use super::RuntimeMemoryStore;
use super::StateRuntime;
use codex_protocol::MemoryVersion;
use codex_protocol::ThreadId;
use std::sync::Arc;

impl StateRuntime {
    /// Return the selected durable store for one memory version.
    pub async fn memory_store_for_version(
        &self,
        version: MemoryVersion,
    ) -> anyhow::Result<Arc<dyn RuntimeMemoryStore>> {
        match &self.memory_store_selection {
            MemoryStoreSelection::Local => Ok(Arc::new(self.memories_for_version(version).await?)),
            MemoryStoreSelection::Injected(stores) => Ok(match version {
                MemoryVersion::V1 => Arc::clone(&stores.v1),
                MemoryVersion::V2 => Arc::clone(&stores.v2),
            }),
        }
    }

    /// Mark a thread polluted in the selected local memory version and enqueue
    /// forgetting for that version's completed consolidation, if applicable.
    ///
    /// Injected stores are rejected until memory state and the thread catalog
    /// have a coordinated mutation protocol. This guard does not make injected
    /// stores safe to activate through callers that only log this error.
    pub async fn mark_thread_memory_mode_polluted_for_version(
        &self,
        version: MemoryVersion,
        thread_id: ThreadId,
    ) -> anyhow::Result<bool> {
        if matches!(
            &self.memory_store_selection,
            MemoryStoreSelection::Injected(_)
        ) {
            anyhow::bail!(
                "cannot pollute thread memory mode with injected stores before cross-store coordination"
            );
        }
        self.memory_store_for_version(version)
            .await?
            .mark_thread_memory_mode_polluted(thread_id)
            .await
    }

    pub async fn memories_for_version(
        &self,
        version: MemoryVersion,
    ) -> anyhow::Result<MemoryStore> {
        match version {
            MemoryVersion::V1 => Ok(self.memories.clone()),
            MemoryVersion::V2 => self
                .memories_v2
                .get_or_try_init(|| async {
                    let pool = self.sqlite.open_memories_v2_db().await?;
                    Ok(MemoryStore::new(Arc::new(pool), Arc::clone(&self.pool)))
                })
                .await
                .cloned(),
        }
    }

    pub async fn clear_all_memory_data(&self) -> anyhow::Result<()> {
        self.memory_store_for_version(MemoryVersion::V1)
            .await?
            .clear_memory_data()
            .await?;
        if matches!(
            &self.memory_store_selection,
            MemoryStoreSelection::Injected(_)
        ) || tokio::fs::try_exists(self.sqlite.memories_v2_db_path()).await?
        {
            self.memory_store_for_version(MemoryVersion::V2)
                .await?
                .clear_memory_data()
                .await?;
        }
        Ok(())
    }

    pub(super) async fn delete_versioned_thread_memory(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<()> {
        self.memory_store_for_version(MemoryVersion::V1)
            .await?
            .delete_thread_memory(thread_id)
            .await?;
        if matches!(
            &self.memory_store_selection,
            MemoryStoreSelection::Injected(_)
        ) || tokio::fs::try_exists(self.sqlite.memories_v2_db_path()).await?
        {
            self.memory_store_for_version(MemoryVersion::V2)
                .await?
                .delete_thread_memory(thread_id)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "memory_versions_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "memory_store_injection_tests.rs"]
mod injection_tests;
