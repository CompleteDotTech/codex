# Native compressed-header helper (partial issue #2)

`codex-rollout-header-preview --snapshot-home HOME` accepts at most 1,024
newline-delimited JSON requests on stdin. Each request has exactly one field:
`{"relative_path":"sessions/YYYY/MM/DD/rollout-...jsonl.zst"}` or an
`archived_sessions/rollout-...jsonl.zst` path. Lines are limited to 4,096
bytes. The whole batch is validated before any per-file response is written.

Each response is one path-free JSON line with either `status=ok`, `thread_id`
and `ancestor_rollout_id` (null when absent), or `status=unresolved` and a
stable code. Bad batch input emits only `status=rejected` with a stable code.
The helper rejects traversal, symlink/reparse components, and nonregular files.
It reads the physical compressed path even when a plain sibling exists. It
limits compressed bytes read to 1 MiB, decoded prefix to 64 KiB, and zstd
window to 8 MiB. A valid header in that prefix can be reported even if the
whole file is larger; the response does not verify the rest of the file.

This helper is not yet called by `preview_rollout_snapshot.py`. Its response
does not establish an immutable snapshot, choose an authoritative copy, prove
whole-file integrity or lineage closure, or permit activation. Concurrent path
replacement remains outside its guarantee until snapshot fencing exists.
