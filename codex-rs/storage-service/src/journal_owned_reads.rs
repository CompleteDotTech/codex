use super::*;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
impl Journal {
    pub(super) fn read_owned_record(
        &self,
        lease: &namespace::Namespace,
        name: &OsStr,
        expected: Uuid,
        mut file: std::fs::File,
        limit: usize,
    ) -> io::Result<(OperationRecord, usize)> {
        let before = file.metadata()?;
        namespace::Namespace::validate_file(&file)?;
        if before.len() > limit as u64 {
            return Err(invalid_record("operation record byte limit"));
        }
        let mut bytes = Vec::new();
        (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        lease.verify_file(name, &file)?;
        if bytes.len() > limit
            || bytes.len() as u64 != before.len()
            || before.len() != after.len()
            || before.modified()? != after.modified()?
            || before.dev() != after.dev()
            || before.ino() != after.ino()
        {
            return Err(invalid_record("operation record changed or byte limit"));
        }
        let record: OperationRecord =
            serde_json::from_slice(&bytes).map_err(|_| invalid_record("operation record"))?;
        if record.operation_id != expected {
            return Err(invalid_record("operation record identity"));
        }
        Ok((record, bytes.len()))
    }
    pub(super) fn read_owned(&self, operation_id: Uuid) -> io::Result<Option<OperationRecord>> {
        let lease = match self.owned_namespace(false) {
            Ok(lease) => lease,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let name = format!("{operation_id}.json");
        let file = match lease.open_regular(OsStr::new(&name), libc::O_RDONLY) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                lease.revalidate()?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        self.read_owned_record(
            &lease,
            OsStr::new(&name),
            operation_id,
            file,
            RECORD_BYTE_LIMIT,
        )
        .map(|(record, _)| Some(record))
    }
    pub(super) fn list_owned(&self) -> io::Result<Vec<OperationRecord>> {
        let lease = match self.owned_namespace(false) {
            Ok(lease) => lease,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        let mut owners = std::collections::HashSet::new();
        let mut bytes = 0_usize;
        for name in lease.entries(RECORD_COUNT_LIMIT)? {
            let path = std::path::Path::new(&name);
            if !path
                .extension()
                .and_then(|part| part.to_str())
                .is_some_and(|part| part.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|part| part.to_str())
                .and_then(|part| Uuid::parse_str(part).ok())
                .ok_or_else(|| invalid_record("operation record filename"))?;
            if name.to_str() != Some(format!("{id}.json").as_str()) {
                return Err(invalid_record("operation record filename"));
            }
            let file = lease.open_regular(&name, libc::O_RDONLY)?;
            let (record, size) = self.read_owned_record(
                &lease,
                &name,
                id,
                file,
                (JOURNAL_BYTE_LIMIT - bytes).min(RECORD_BYTE_LIMIT),
            )?;
            bytes = bytes
                .checked_add(size)
                .ok_or_else(|| invalid_record("journal byte overflow"))?;
            if bytes > JOURNAL_BYTE_LIMIT || !owners.insert(id) {
                return Err(invalid_record("journal byte limit or duplicate owner"));
            }
            records.push(record);
        }
        lease.revalidate()?;
        records.sort_by_key(|record| (record.created_at_ms, record.operation_id));
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> OperationRecord {
        OperationRecord {
            operation_id: Uuid::new_v4(),
            action: PlanAction::Return,
            plan_digest: "owned".into(),
            state: OperationState::Planned,
            run_id: None,
            return_source: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            blocker: None,
            copied: Vec::new(),
        }
    }
    #[test]
    fn repeated_cached_listing_then_new_updated_owner_never_reports_false_idle() {
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let first = record();
        journal.create(&first).unwrap();
        for _ in 0..2 {
            assert_eq!(journal.list_checked().unwrap(), vec![first.clone()]);
        }
        let mut second = record();
        second.operation_id = Uuid::from_u128(first.operation_id.as_u128() ^ 1);
        journal.create(&second).unwrap();
        second.state = OperationState::Ready;
        second.updated_at_ms += 1;
        journal.update(&second).unwrap();
        let mut expected = vec![first, second];
        expected.sort_by_key(|value| (value.created_at_ms, value.operation_id));
        assert_eq!(journal.list_checked().unwrap(), expected);
        assert_eq!(journal.list_checked().unwrap(), expected);
    }
    #[test]
    fn cached_namespace_disappearance_is_refusal_not_idle_or_absence() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let journal = Journal::new(&home);
        let value = record();
        journal.create(&value).unwrap();
        assert_eq!(
            journal.read(value.operation_id).unwrap(),
            Some(value.clone())
        );
        assert_eq!(journal.list_checked().unwrap(), vec![value.clone()]);
        std::fs::rename(&home, root.path().join("retained-original")).unwrap();
        assert!(journal.read(value.operation_id).is_err());
        assert!(journal.list_checked().is_err());
        assert!(journal.list().is_empty()); // Display only; never an idle authority receipt.
        assert_eq!(Journal::new(&home).read(value.operation_id).unwrap(), None);
    }
    #[test]
    fn retained_descriptor_replacement_cannot_adopt_foreign_record() {
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let value = record();
        journal.create(&value).unwrap();
        let lease = journal.owned_namespace(false).unwrap();
        let name = format!("{}.json", value.operation_id);
        let held = lease
            .open_regular(OsStr::new(&name), libc::O_RDONLY)
            .unwrap();
        std::fs::rename(
            journal.path(value.operation_id),
            home.path().join("original.json"),
        )
        .unwrap();
        std::fs::write(
            journal.path(value.operation_id),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(
            journal
                .read_owned_record(
                    &lease,
                    OsStr::new(&name),
                    value.operation_id,
                    held,
                    RECORD_BYTE_LIMIT
                )
                .is_err()
        );
        assert!(home.path().join("original.json").exists());
    }
}
