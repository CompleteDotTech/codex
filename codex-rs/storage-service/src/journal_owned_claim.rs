use super::*;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Read;
use std::sync::Arc;
use std::sync::Weak;
#[derive(Clone)]
pub(crate) struct ReturnClaim(Arc<ClaimInner>);
pub(super) struct ClaimInner {
    lease: Arc<namespace::Namespace>,
    lock_name: OsString,
    file: std::fs::File,
    original: OperationRecord,
    encoded_bytes: usize,
}
impl ReturnClaim {
    pub(crate) fn revalidate(&self) -> io::Result<()> {
        self.0.revalidate()
    }
}
impl ClaimInner {
    fn revalidate(&self) -> io::Result<()> {
        self.lease.verify_file(&self.lock_name, &self.file)?;
        let name = format!("{}.json", self.original.operation_id);
        let mut file = self.lease.open_regular(OsStr::new(&name), libc::O_RDONLY)?;
        let before = file.metadata()?;
        if before.len() > RECORD_BYTE_LIMIT as u64 {
            return Err(invalid_record("claimed record byte limit"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(RECORD_BYTE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        self.lease.verify_file(OsStr::new(&name), &file)?;
        if bytes.len() > RECORD_BYTE_LIMIT
            || bytes.len() as u64 != before.len()
            || before.len() != after.len()
            || before.modified()? != after.modified()?
        {
            return Err(invalid_record("claimed record changed or byte limit"));
        }
        let current: OperationRecord =
            serde_json::from_slice(&bytes).map_err(|_| invalid_record("claimed record"))?;
        self.admit_record(&current)
    }
    fn admit_record(&self, current: &OperationRecord) -> io::Result<()> {
        if current.operation_id != self.original.operation_id
            || current.action != self.original.action
            || current.plan_digest != self.original.plan_digest
            || current.run_id != self.original.run_id
            || current.return_source != self.original.return_source
            || current.created_at_ms != self.original.created_at_ms
        {
            return Err(invalid_record("claimed record owner tuple changed"));
        }
        Ok(())
    }
}
impl Journal {
    pub(super) fn claim_owned(&self, operation_id: Uuid) -> io::Result<ReturnClaim> {
        let lease = self.owned_namespace(false)?;
        let name = format!("{operation_id}.json");
        let record_file = lease.open_regular(OsStr::new(&name), libc::O_RDONLY)?;
        let (original, encoded_bytes) = self.read_owned_record(
            &lease,
            OsStr::new(&name),
            operation_id,
            record_file,
            RECORD_BYTE_LIMIT,
        )?;
        let lock_name = OsString::from(format!("{operation_id}.lock"));
        let file = lease.open_regular(&lock_name, libc::O_RDWR | libc::O_CREAT)?;
        file.try_lock().map_err(io::Error::other)?;
        lease.verify_file(&lock_name, &file)?;
        let inner = Arc::new(ClaimInner {
            lease,
            lock_name,
            file,
            original,
            encoded_bytes,
        });
        inner.revalidate()?;
        let mut claims = self
            .claims
            .lock()
            .map_err(|_| invalid_record("journal claims poisoned"))?;
        claims.retain(|_, weak| weak.strong_count() != 0);
        let bytes =
            claims
                .values()
                .filter_map(Weak::upgrade)
                .try_fold(0_usize, |total, claim| {
                    total
                        .checked_add(claim.encoded_bytes)
                        .ok_or_else(|| invalid_record("claim byte overflow"))
                })?;
        if encoded_bytes > JOURNAL_BYTE_LIMIT.saturating_sub(bytes) {
            return Err(invalid_record("journal claim byte limit"));
        }
        if claims.len() >= RECORD_COUNT_LIMIT {
            return Err(invalid_record("journal claim count limit"));
        }
        if claims.get(&operation_id).and_then(Weak::upgrade).is_some() {
            return Err(invalid_record("operation already claimed"));
        }
        claims.insert(operation_id, Arc::downgrade(&inner));
        Ok(ReturnClaim(inner))
    }
    pub(super) fn validate_claim(&self, operation_id: Uuid) -> io::Result<()> {
        let inner = self
            .claims
            .lock()
            .map_err(|_| invalid_record("journal claims poisoned"))?
            .get(&operation_id)
            .and_then(Weak::upgrade);
        if let Some(inner) = inner {
            inner.revalidate()?;
        }
        Ok(())
    }
    pub(super) fn admit_candidate(&self, record: &OperationRecord) -> io::Result<()> {
        let inner = self
            .claims
            .lock()
            .map_err(|_| invalid_record("journal claims poisoned"))?
            .get(&record.operation_id)
            .and_then(Weak::upgrade);
        if let Some(inner) = inner {
            inner.revalidate()?;
            inner.admit_record(record)?;
        }
        Ok(())
    }
    pub(super) fn validate_claims(&self) -> io::Result<()> {
        let claims: Vec<_> = self
            .claims
            .lock()
            .map_err(|_| invalid_record("journal claims poisoned"))?
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for claim in claims {
            claim.revalidate()?;
        }
        Ok(())
    }
    pub(crate) fn validate_return_claim(&self, claim: &ReturnClaim) -> io::Result<()> {
        let namespace = self.owned_namespace(false)?;
        if !Arc::ptr_eq(&namespace, &claim.0.lease) {
            return Err(invalid_record("claim belongs to another journal namespace"));
        }
        claim.revalidate()
    }
}

impl codex_storage_migration::ReturnOperationFence for ReturnClaim {
    fn check_current(&self) -> Result<(), codex_storage_migration::MigrationError> {
        let run = self
            .0
            .original
            .run_id
            .ok_or(codex_storage_migration::MigrationError::TargetBusy)?;
        codex_storage_migration::ReturnOperationFence::check(self, run).map(|_| ())
    }
    fn check(
        &self,
        run_id: Uuid,
    ) -> Result<codex_storage_migration::ActivationTarget, codex_storage_migration::MigrationError>
    {
        use codex_storage_migration::ActivationTarget;
        use codex_storage_migration::MigrationError;
        self.revalidate()
            .map_err(|_| MigrationError::Staging("operation ownership evidence changed".into()))?;
        if self.0.original.action != PlanAction::Return || self.0.original.run_id != Some(run_id) {
            return Err(MigrationError::TargetBusy);
        }
        let source = self
            .0
            .original
            .return_source
            .as_ref()
            .ok_or(MigrationError::TargetBusy)?;
        if source.generation < 0 {
            return Err(MigrationError::TargetBusy);
        }
        Ok(ActivationTarget {
            dataset_id: source.dataset_id,
            generation: source.generation,
        })
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
            run_id: Some(Uuid::new_v4()),
            return_source: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            blocker: None,
            copied: Vec::new(),
        }
    }
    #[test]
    fn migration_fence_requires_original_run_source_and_retained_lock() {
        use codex_storage_migration::ReturnOperationFence;
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let mut value = record();
        let run = value.run_id.unwrap();
        let dataset = Uuid::new_v4();
        value.return_source = Some(ReturnSource {
            dataset_id: dataset,
            generation: 7,
            destination: "owned destination".into(),
        });
        journal.create(&value).unwrap();
        let claim = journal.claim_return(value.operation_id).unwrap();
        let admitted = claim.check(run).unwrap();
        assert_eq!(admitted.dataset_id, dataset);
        assert_eq!(admitted.generation, 7);
        claim.check_current().unwrap();
        assert!(claim.check(Uuid::new_v4()).is_err());
        claim.check(run).unwrap();
        let bytes = std::fs::read(journal.path(value.operation_id)).unwrap();
        let lock = journal.path(value.operation_id).with_extension("lock");
        std::fs::rename(&lock, lock.with_extension("original-lock")).unwrap();
        std::fs::write(&lock, b"foreign lock").unwrap();
        assert!(claim.check(run).is_err());
        assert!(claim.check_current().is_err());
        assert_eq!(std::fs::read(&lock).unwrap(), b"foreign lock");
        assert_eq!(
            std::fs::read(journal.path(value.operation_id)).unwrap(),
            bytes
        );
    }
    #[test]
    fn migration_fence_does_not_mint_legacy_missing_source_presence() {
        use codex_storage_migration::ReturnOperationFence;
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let value = record();
        assert!(value.return_source.is_none());
        journal.create(&value).unwrap();
        let before = std::fs::read(journal.path(value.operation_id)).unwrap();
        let claim = journal.claim_return(value.operation_id).unwrap();
        assert!(claim.check(value.run_id.unwrap()).is_err());
        assert!(claim.check_current().is_err());
        claim.revalidate().unwrap();
        assert_eq!(
            std::fs::read(journal.path(value.operation_id)).unwrap(),
            before
        );
        assert_eq!(journal.list_checked().unwrap(), vec![value]);
    }
    #[test]
    fn public_update_refuses_foreign_candidate_before_any_journal_publication() {
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observer = Arc::clone(&seen);
        let journal = Journal::new(home.path()).observed_by(Arc::new(move |_| {
            observer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let mut original = record();
        journal.create(&original).unwrap();
        let claim = journal.claim_return(original.operation_id).unwrap();
        let bytes = std::fs::read(journal.path(original.operation_id)).unwrap();
        for field in 0..3 {
            let mut foreign = original.clone();
            match field {
                0 => foreign.run_id = Some(Uuid::new_v4()),
                1 => foreign.plan_digest = "foreign plan".into(),
                _ => {
                    foreign.return_source = Some(ReturnSource {
                        dataset_id: Uuid::new_v4(),
                        generation: 2,
                        destination: "foreign destination".into(),
                    })
                }
            }
            assert!(journal.update(&foreign).is_err());
            assert_eq!(
                std::fs::read(journal.path(original.operation_id)).unwrap(),
                bytes
            );
            assert_eq!(journal.list_checked().unwrap(), vec![original.clone()]);
            assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(
                !std::fs::read_dir(&journal.directory)
                    .unwrap()
                    .any(|entry| entry
                        .unwrap()
                        .path()
                        .extension()
                        .is_some_and(|part| part == "tmp"))
            );
            claim.revalidate().unwrap();
        }
        original.state = OperationState::Ready;
        original.updated_at_ms += 1;
        journal.update(&original).unwrap();
        claim.revalidate().unwrap();
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(journal.list_checked().unwrap(), vec![original]);
    }
    #[test]
    fn replacement_lock_cannot_be_claimed_as_same_live_operation_or_hide_idle() {
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let value = record();
        journal.create(&value).unwrap();
        let claim = journal.claim_return(value.operation_id).unwrap();
        journal.validate_return_claim(&claim).unwrap();
        assert!(journal.claim_return(value.operation_id).is_err());
        let name = journal.path(value.operation_id).with_extension("lock");
        std::fs::rename(&name, name.with_extension("retained-lock")).unwrap();
        std::fs::write(&name, b"replacement").unwrap();
        assert!(claim.revalidate().is_err());
        assert!(journal.read(value.operation_id).is_err());
        assert!(journal.list_checked().is_err());
        assert!(journal.update(&value).is_err());
        assert!(journal.claim_return(value.operation_id).is_err());
        assert_eq!(std::fs::read(&name).unwrap(), b"replacement");
    }
    #[test]
    fn clone_retains_original_lock_until_last_owner_and_state_updates_keep_tuple() {
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let mut value = record();
        journal.create(&value).unwrap();
        let first = journal.claim_return(value.operation_id).unwrap();
        assert!(
            Journal::new(home.path())
                .validate_return_claim(&first)
                .is_err()
        );
        let retained = first.clone();
        drop(first);
        assert!(journal.claim_return(value.operation_id).is_err());
        value.state = OperationState::Ready;
        value.updated_at_ms += 1;
        journal.update(&value).unwrap();
        retained.revalidate().unwrap();
        assert_eq!(journal.list_checked().unwrap(), vec![value.clone()]);
        drop(retained);
        journal
            .claim_return(value.operation_id)
            .unwrap()
            .revalidate()
            .unwrap();
    }
    #[test]
    fn lock_hardlink_and_record_owner_tuple_tamper_are_not_adopted() {
        let home = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        let mut value = record();
        journal.create(&value).unwrap();
        let claim = journal.claim_return(value.operation_id).unwrap();
        let lock = journal.path(value.operation_id).with_extension("lock");
        let alias = home.path().join("lock-alias");
        std::fs::hard_link(&lock, &alias).unwrap();
        assert!(claim.revalidate().is_err());
        std::fs::remove_file(alias).unwrap();
        claim.revalidate().unwrap();
        value.run_id = Some(Uuid::new_v4());
        let changed = serde_json::to_vec(&value).unwrap();
        std::fs::write(journal.path(value.operation_id), &changed).unwrap();
        assert!(claim.revalidate().is_err());
        assert!(journal.read(value.operation_id).is_err());
        assert!(journal.list_checked().is_err());
        assert_eq!(
            std::fs::read(journal.path(value.operation_id)).unwrap(),
            changed
        );
    }
}
