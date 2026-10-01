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

`preview_rollout_snapshot.py --snapshot-home HOME --compressed-header-helper
ABSOLUTE_BINARY_PATH` optionally sends one bounded batch of compressed paths to
this helper. The binary path is supplied explicitly; the preview does not search
`PATH` or build it. The Python preview accepts only a complete, strictly shaped
response batch with matching filename thread IDs. A failed, timed-out, oversized,
or malformed batch leaves every compressed header unknown and reports a stable,
path-free code. More than 1,024 compressed candidates are left unknown.

Neither helper nor preview establishes an immutable snapshot, chooses an
authoritative copy, proves whole-file integrity or lineage closure, or permits
activation. Concurrent path replacement remains outside their guarantee until
snapshot fencing exists.
