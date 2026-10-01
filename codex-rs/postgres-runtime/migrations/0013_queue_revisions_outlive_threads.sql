-- Queue change records outlive their thread. A watcher that last saw an older revision must
-- still find the deletion, so the revision row cannot cascade away with the thread row.
ALTER TABLE codex_storage.queued_thread_revisions
    DROP CONSTRAINT queued_thread_revisions_thread_id_fkey;

UPDATE codex_storage.codex_schema_meta
SET format_version = 13, min_reader_version = 13, min_writer_version = 13
WHERE singleton = TRUE;
