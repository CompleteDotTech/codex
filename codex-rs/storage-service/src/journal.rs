//! Durable operation records, so a restart or a reconnecting client can find an operation again
//! and a repeated request never starts a second copy of it.

use crate::BlockerCode;
use crate::PlanAction;
use serde::Deserialize;
use serde::Serialize;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use uuid::Uuid;

const DIRECTORY: &str = "storage-operations";

/// Where an operation is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Planned,
    Copying,
    Verifying,
    /// The copy is verified and waiting for the explicit step that makes it authoritative.
    Ready,
    Committing,
    Active,
    Failed,
    Cancelled,
}

impl OperationState {
    /// Whether the operation can still change without a new request.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Active | Self::Failed | Self::Cancelled)
    }
}

/// One operation as it is stored and reported.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ReturnSource {
    pub dataset_id: Uuid,
    pub generation: i64,
    /// Credential-free endpoint/database/namespace selected by the confirmed plan.
    pub destination: String,
}

/// One operation as it is stored and reported.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct OperationRecord {
    pub operation_id: Uuid,
    pub action: PlanAction,
    /// The plan the operator confirmed; a changed world makes it stale.
    pub plan_digest: String,
    pub state: OperationState,
    /// The migration run in the destination, once one exists.
    pub run_id: Option<Uuid>,
    /// Old journals without source ownership cannot safely resume or cancel an export.
    #[serde(default)]
    pub return_source: Option<ReturnSource>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub blocker: Option<BlockerCode>,
    /// Rows copied so far, by domain name.
    pub copied: Vec<(String, u64)>,
}

/// The operation records of one home.
pub(crate) struct Journal {
    directory: PathBuf,
    observer: Option<Observer>,
    #[cfg(all(test, unix))]
    sync_probe: Option<std::sync::Arc<std::sync::Mutex<SyncProbe>>>,
}

/// Called with every record the journal durably writes.
pub type Observer = std::sync::Arc<dyn Fn(&OperationRecord) + Send + Sync>;

impl Journal {
    /// Cooperative cross-process exclusion for an operation's export and cancellation.
    /// The persistent lock file is never removed; closing the handle releases ownership.
    pub(crate) fn claim_return(&self, operation_id: Uuid) -> io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path(operation_id).with_extension("lock"))?;
        file.try_lock().map_err(io::Error::other)?;
        Ok(file)
    }

    pub(crate) fn new(codex_home: &std::path::Path) -> Self {
        Self {
            directory: codex_home.join(DIRECTORY),
            observer: None,
            #[cfg(all(test, unix))]
            sync_probe: None,
        }
    }

    pub(crate) fn observed_by(mut self, observer: Observer) -> Self {
        self.observer = Some(observer);
        self
    }

    fn notify(&self, record: &OperationRecord) {
        if let Some(observer) = &self.observer {
            observer(record);
        }
    }

    fn path(&self, operation_id: Uuid) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    /// Record a new operation; fails if the id is already taken.
    pub(crate) fn create(&self, record: &OperationRecord) -> io::Result<()> {
        #[cfg(unix)]
        self.create_durable_directory()?;
        #[cfg(not(unix))]
        std::fs::create_dir_all(&self.directory)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(record.operation_id))?;
        serde_json::to_writer(&mut file, record).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        #[cfg(unix)]
        self.sync(&file, &self.path(record.operation_id), "record-file")?;
        #[cfg(not(unix))]
        file.sync_all()?;
        #[cfg(unix)]
        self.sync(
            &std::fs::File::open(&self.directory)?,
            &self.directory,
            "journal-parent",
        )?;
        self.notify(record);
        Ok(())
    }

    /// Replace a record atomically.
    pub(crate) fn update(&self, record: &OperationRecord) -> io::Result<()> {
        let path = self.path(record.operation_id);
        let temporary = path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&temporary)?;
        serde_json::to_writer(&mut file, record).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        #[cfg(unix)]
        self.sync(&file, &temporary, "update-file")?;
        #[cfg(not(unix))]
        file.sync_all()?;
        std::fs::rename(temporary, path)?;
        #[cfg(unix)]
        self.sync(
            &std::fs::File::open(&self.directory)?,
            &self.directory,
            "journal-parent",
        )?;
        self.notify(record);
        Ok(())
    }

    pub(crate) fn read(&self, operation_id: Uuid) -> io::Result<Option<OperationRecord>> {
        match std::fs::read(self.path(operation_id)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "operation record")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Recovery must distinguish an absent journal from unreadable ownership evidence.
    pub(crate) fn list_checked(&self) -> io::Result<Vec<OperationRecord>> {
        match std::fs::symlink_metadata(&self.directory) {
            Ok(metadata) if metadata.file_type().is_dir() && !is_redirected(&metadata) => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "operation journal is not a directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        }
        let mut records = Vec::new();
        let mut owners = std::collections::HashSet::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let path = entry.path();
            if !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() || is_redirected(&metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "operation record is not regular",
                ));
            }
            let operation_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| Uuid::parse_str(name).ok())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "operation record filename")
                })?;
            // Readers and writers use this exact spelling. Accepting aliases on a
            // case-sensitive directory could make an update create a second owner.
            if path.file_name().and_then(|name| name.to_str())
                != Some(format!("{operation_id}.json").as_str())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "operation record filename",
                ));
            }
            let record: OperationRecord = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "operation record"))?;
            if record.operation_id != operation_id || !owners.insert(operation_id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "operation record identity",
                ));
            }
            records.push(record);
        }
        records.sort_by_key(|record| (record.created_at_ms, record.operation_id));
        Ok(records)
    }

    /// Best-effort listing for display only. Never use this to decide recovery is idle.
    pub(crate) fn list(&self) -> Vec<OperationRecord> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        let mut records: Vec<OperationRecord> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
            .collect();
        records.sort_by_key(|record| (record.created_at_ms, record.operation_id));
        records
    }
}

#[cfg(windows)]
fn is_redirected(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT includes junctions that still report directory type.
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(unix)]
impl Journal {
    fn sync(
        &self,
        file: &std::fs::File,
        path: &std::path::Path,
        phase: &'static str,
    ) -> io::Result<()> {
        #[cfg(test)]
        self.crash_boundary(phase, false)?;
        #[cfg(test)]
        if let Some(probe) = &self.sync_probe {
            let mut probe = probe
                .lock()
                .map_err(|_| io::Error::other("sync probe poisoned"))?;
            probe.events.push((phase, path.to_path_buf()));
            if probe.fail == Some(phase) {
                probe.fail = None;
                return Err(io::Error::other("injected sync refusal"));
            }
        }
        #[cfg(not(test))]
        let _ = (path, phase);
        file.sync_all()?;
        #[cfg(test)]
        self.crash_boundary(phase, true)?;
        Ok(())
    }
    #[cfg(test)]
    fn crash_boundary(&self, phase: &'static str, after: bool) -> io::Result<()> {
        if let Some(probe) = &self.sync_probe {
            let probe = probe
                .lock()
                .map_err(|_| io::Error::other("sync probe poisoned"))?;
            if let Some((expected, expected_after, witness)) = &probe.crash
                && *expected == phase
                && *expected_after == after
            {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(witness)?;
                write!(file, "{phase}:{after}")?;
                file.sync_all()?;
                // Process-crash control: no stack unwinding or observer notification.
                std::process::exit(87);
            }
        }
        Ok(())
    }
    fn create_durable_directory(&self) -> io::Result<()> {
        let absolute = std::path::absolute(&self.directory)?;
        std::fs::create_dir_all(&absolute)?;
        for parent in absolute.ancestors().skip(1) {
            self.sync(&std::fs::File::open(parent)?, parent, "ancestor-parent")?;
        }
        Ok(())
    }
}
#[cfg(all(test, unix))]
#[derive(Default)]
struct SyncProbe {
    fail: Option<&'static str>,
    events: Vec<(&'static str, PathBuf)>,
    crash: Option<(&'static str, bool, PathBuf)>,
}

#[cfg(not(windows))]
fn is_redirected(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "journal_durability_tests.rs"]
mod durability_tests;

#[cfg(all(test, unix))]
#[path = "journal_crash_tests.rs"]
mod crash_tests;
