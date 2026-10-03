use super::*;
use std::ffi::OsStr;
use std::io::Write;
use std::sync::Arc;
impl Journal {
    pub(super) fn owned_namespace(&self, create: bool) -> io::Result<Arc<namespace::Namespace>> {
        let mut binding = self
            .namespace
            .lock()
            .map_err(|_| invalid_record("journal namespace binding poisoned"))?;
        if let Some(lease) = &*binding {
            lease.revalidate().map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    invalid_record("bound journal namespace disappeared")
                } else {
                    error
                }
            })?;
            return Ok(Arc::clone(lease));
        }
        let lease = Arc::new(namespace::Namespace::acquire(&self.directory, create)?);
        *binding = Some(Arc::clone(&lease));
        Ok(lease)
    }
    fn persist_owned(
        &self,
        lease: &namespace::Namespace,
        name: &OsStr,
        file: &mut std::fs::File,
        encoded: &[u8],
        phase: &'static str,
    ) -> io::Result<()> {
        file.write_all(encoded)?;
        lease.verify_file(name, file)?;
        self.sync(file, &self.directory.join(name), phase)?;
        lease.verify_file(name, file)
    }
    pub(super) fn create_owned(&self, record: &OperationRecord) -> io::Result<()> {
        let encoded = encode_record(record)?;
        let lease = self.owned_namespace(true)?;
        let absolute = std::path::absolute(&self.directory)?;
        for (parent, path) in lease.ancestors().zip(absolute.ancestors().skip(1)) {
            self.sync(parent, path, "ancestor-parent")?;
        }
        lease.revalidate()?;
        let name = format!("{}.json", record.operation_id);
        let mut file = lease.open_regular(
            OsStr::new(&name),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        self.persist_owned(
            &lease,
            OsStr::new(&name),
            &mut file,
            &encoded,
            "record-file",
        )?;
        self.sync(lease.leaf()?, &self.directory, "journal-parent")?;
        lease.verify_file(OsStr::new(&name), &file)?;
        self.notify(record);
        Ok(())
    }
    pub(super) fn update_owned(&self, record: &OperationRecord) -> io::Result<()> {
        let encoded = encode_record(record)?;
        let lease = self.owned_namespace(false)?;
        let target = format!("{}.json", record.operation_id);
        let original = lease.open_regular(OsStr::new(&target), libc::O_RDONLY)?;
        self.read_owned_record(
            &lease,
            OsStr::new(&target),
            record.operation_id,
            original.try_clone()?,
            RECORD_BYTE_LIMIT,
        )?;
        lease.verify_file(OsStr::new(&target), &original)?;
        let temporary = format!("{}.{}.json.tmp", record.operation_id, Uuid::new_v4());
        let mut file = lease.open_regular(
            OsStr::new(&temporary),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        self.persist_owned(
            &lease,
            OsStr::new(&temporary),
            &mut file,
            &encoded,
            "update-file",
        )?;
        lease.verify_file(OsStr::new(&target), &original)?;
        lease.rename(OsStr::new(&temporary), &file, OsStr::new(&target))?;
        self.sync(lease.leaf()?, &self.directory, "journal-parent")?;
        lease.verify_file(OsStr::new(&target), &file)?;
        self.notify(record);
        Ok(())
    }
}
fn encode_record(record: &OperationRecord) -> io::Result<Vec<u8>> {
    let mut writer = BoundedRecord(Vec::new());
    serde_json::to_writer(&mut writer, record).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    Ok(writer.0)
}
struct BoundedRecord(Vec<u8>);
impl Write for BoundedRecord {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > RECORD_BYTE_LIMIT.saturating_sub(self.0.len()) {
            return Err(invalid_record("operation record too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversize_create_and_update_never_publish_partial_canonical_or_observer() {
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observer = Arc::clone(&seen);
        let journal = Journal::new(home.path()).observed_by(Arc::new(move |_| {
            observer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let mut record = OperationRecord {
            operation_id: Uuid::new_v4(),
            action: PlanAction::Return,
            plan_digest: "x".repeat(RECORD_BYTE_LIMIT),
            state: OperationState::Planned,
            run_id: None,
            return_source: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            blocker: None,
            copied: Vec::new(),
        };
        assert!(journal.create(&record).is_err());
        assert!(!journal.directory.exists());
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 0);
        record.plan_digest = "bounded".into();
        journal.create(&record).unwrap();
        let original = std::fs::read(journal.path(record.operation_id)).unwrap();
        let original_record = record.clone();
        record.plan_digest = "\0".repeat(RECORD_BYTE_LIMIT / 2);
        assert!(journal.update(&record).is_err());
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read(journal.path(record.operation_id)).unwrap(),
            original
        );
        assert_eq!(journal.list_checked().unwrap(), vec![original_record]);
        assert!(
            !std::fs::read_dir(&journal.directory).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|value| value == "tmp")
            })
        );
    }
    #[test]
    fn retained_ancestor_replacement_and_symlink_cannot_redirect_write() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let lease = namespace::Namespace::acquire(&home.join(DIRECTORY), true).unwrap();
        let file = lease
            .open_regular(
                OsStr::new("owned.json"),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )
            .unwrap();
        let moved = root.path().join("original");
        std::fs::rename(&home, &moved).unwrap();
        let foreign = root.path().join("foreign");
        std::fs::create_dir_all(foreign.join(DIRECTORY)).unwrap();
        std::os::unix::fs::symlink(&foreign, &home).unwrap();
        assert!(lease.revalidate().is_err());
        assert!(
            lease
                .open_regular(
                    OsStr::new("new.json"),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
                )
                .is_err()
        );
        assert!(lease.verify_file(OsStr::new("owned.json"), &file).is_err());
        assert!(!foreign.join(DIRECTORY).join("new.json").exists());
        assert!(moved.join(DIRECTORY).join("owned.json").exists());
    }
    #[test]
    fn hardlink_and_named_file_replacement_refuse_original_identity() {
        let root = tempfile::tempdir().unwrap();
        let lease = namespace::Namespace::acquire(root.path(), false).unwrap();
        let file = lease
            .open_regular(
                OsStr::new("owned"),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            )
            .unwrap();
        std::fs::hard_link(root.path().join("owned"), root.path().join("alias")).unwrap();
        assert!(lease.verify_file(OsStr::new("owned"), &file).is_err());
        assert!(
            lease
                .open_regular(OsStr::new("alias"), libc::O_RDONLY)
                .is_err()
        );
        std::fs::remove_file(root.path().join("alias")).unwrap();
        std::fs::rename(root.path().join("owned"), root.path().join("original")).unwrap();
        std::fs::write(root.path().join("owned"), b"foreign replacement").unwrap();
        assert!(lease.verify_file(OsStr::new("owned"), &file).is_err());
        assert_eq!(
            std::fs::read(root.path().join("owned")).unwrap(),
            b"foreign replacement"
        );
    }
    #[test]
    fn exclusive_temp_collision_preserves_existing_file_and_namespace() {
        let root = tempfile::tempdir().unwrap();
        let lease = namespace::Namespace::acquire(root.path(), false).unwrap();
        let file = lease
            .open_regular(
                OsStr::new("record.json.tmp"),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )
            .unwrap();
        assert!(
            lease
                .open_regular(
                    OsStr::new("record.json.tmp"),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
                )
                .is_err()
        );
        assert!(
            lease
                .verify_file(OsStr::new("record.json.tmp"), &file)
                .is_ok()
        );
        assert_eq!(file.metadata().unwrap().len(), 0);
    }
}
