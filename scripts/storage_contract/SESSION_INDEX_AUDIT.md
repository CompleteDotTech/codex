# Name-index snapshot audit — partial issue #2

Developer-only, read-only inspection of a protected offline `session_index.jsonl`
capture. This does not export, reconcile names, select storage, migrate a live
home, or replace native Rust/process/database qualification. Every success reports
`activation_permitted: false`; no dependent issue is released.

## Source contract

Pinned to fork `395c622d693cf8ec1c4769cf71bc4a949da3e017`:
- [SessionIndexEntry and readers](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/rollout/src/session_index.rs#L21-L163): `id`, `thread_name`, `updated_at`; ordered appends, different single/batch handling of clears and whitespace.
- [ThreadId serialization](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/protocol/src/thread_id.rs): producer emits UUID text; native parsing also accepts alternate representations.
- [Metadata writes](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/thread-store/src/local/update_thread_metadata.rs#L1-L880): legacy index-only names and SQLite-owned paginated names require different reconciliation.

This helper accepts only the producer's lowercase hyphenated UUID representation,
exactly those three UTF-8 string fields, newline-terminated JSON records, and blank
lines containing JSON whitespace. It deliberately rejects unknown/duplicate keys,
malformed input, alternate UUID spellings, and unpaired surrogate values rather
than silently dropping them. Native readers are more permissive: rejection here
requires review, not deletion, normalization, or a claim of native corruption.
`updated_at` remains an opaque string, including `unknown`; it is not used to sort.

All physical bytes, including blank lines, encoding, order, repeated IDs, empty
names, and original whitespace, enter SHA-256 unchanged. The report contains
counts and the raw digest, never names, IDs, timestamps, paths, or exception chains.
It does not resolve the current name, count distinct threads, prove complete
rollout/artifact coverage, or establish compatibility with any upstream binary.

## Use and limits

From the repository root, provide a digest trusted independently of the capture:

```sh
python scripts/audit_session_index.py --snapshot /protected/captured-index.jsonl \
  --expected-sha256 TRUSTED_CAPTURE_SHA256
python -m unittest discover -s scripts/storage_contract -t scripts -p test_session_index_audit.py -v
```

Exit codes: 0 verified structure/bytes; 2 rejected; 3 unavailable input. Never obtain
the expected digest from the untrusted artifact itself. Metadata checks surround
reads of the opened regular file but do not lock writers or fence path replacement.
Use protected immutable captures; a successful read is not source quiescence proof.

Limits are 1 MiB per physical line and 1 TiB per capture, with bounded line reads and
no dataset-sized name map. These are this audit tool's limits, not Codex runtime
limits or a changed export format. Oversized/partial data is refused, never truncated.
Tests create synthetic disposable indexes and separate Python processes; they do
not execute Rust or prove native consumer parity. Python may create source bytecode
caches. Native reader and cross-version compatibility qualification remain separate
work.
