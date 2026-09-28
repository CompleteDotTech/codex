# Draft storage migration contract audit tool

Partial work for CompleteDotTech/codex issue #2, based on
`985cf47a4eb6084b2ff6b30ebdb1216acda85bb4`.

This is an independently executable **offline format verifier**, not the PostgreSQL
backend, an exporter/importer, the Codex storage control plane, or a cutover gate.
It does not change Codex's configuration or SQLite default. It cannot establish
that an export includes every authoritative source table/file. Its success report
always includes `"activation_permitted": false`.

## Components

- `records.py`: typed canonical JSONL records and ordered logical fingerprints.
- `manifest.py`: strict manifest validation and bounded streaming verification.
- `cli.py` and `../verify_storage_bundle.py`: read-only, payload-free CLI diagnostics.
- `sqlite_snapshot.py` and `../audit_sqlite_snapshot.py`: independently authenticated
  SQLite backup auditing; see `SQLITE_AUDIT.md` for its separate scope and limits.
- `coverage_matrix.py`: checked partial inventory of the eight pinned SQLite
  schemas and canonical file classes. `audit_coverage()` rejects missing or extra
  fixture tables and reports unresolved forward/reverse treatment explicitly.
- `control_plan.py`: a separate, offline operation-plan validator and exclusion
  preview for initialize, local migration, and attachment; see `CONTROL_PLAN.md`.
- `test_*.py`: synthetic bundle tests and disposable SQLite/process-boundary tests.

No third-party Python packages are required. Source syntax targets the enclosing
`scripts/pyproject.toml` minimum of Python 3.10. Executed runtime/platform results
are recorded separately; syntax compatibility is not cross-platform qualification.

## Run tests

From the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts -v
```

The subprocess tests use temporary directories and verify that their input files
are unchanged. The named-pipe test is POSIX-specific and explicitly skipped on
platforms without `os.mkfifo`. No test uses a live Codex home, database or provider.

## Verify a captured artifact

Supply three existing files: the manifest, its concatenated JSONL chunk payload,
and an independently trusted domain inventory. Supply the expected manifest digest
from the trusted capture/confirmed plan, not a digest obtained from an untrusted
export. Paths below are illustrative; no production exporter is implemented here.

```sh
python scripts/verify_storage_bundle.py \
  --manifest capture/manifest.json \
  --payload capture/payload.jsonl \
  --inventory trusted/source-inventory.json \
  --expected-manifest-sha256 TRUSTED_CAPTURE_DIGEST
```

The CLI opens explicitly selected regular files read-only. It does not extract an
archive, resolve paths embedded in records, connect to a database, or modify an
installation. Use immutable operator-owned capture files, never a live SQLite
file or an actively written rollout. Python may create its normal bytecode cache
in the source tree; input artifacts are not changed.

Exit codes: `0` means the supplied bundle passed this verifier; `2` means rejected
input/contract; `3` means an input could not be opened or read. Reports contain
fixed diagnostic codes and aggregate counts, not record values, connection strings,
paths, credential references or exception chains. Argument errors are also redacted.
This does **not** establish that arbitrary input records themselves contain no secrets.

## Trust and coverage

The inventory is a separate trusted input: a list of domain ID, schema and treatment
triples. Its contents must come from the eventual reviewed source inventory/capture.
An exporter cannot silently delete a domain, add an unclassified domain, or change
`migrate` to `retain` without rejection against that independent inventory.

The synthetic record/bundle fixtures name all eight database store categories, several rollout
forms, artifacts, a rebuildable session index, and retained host/lifecycle categories.
They are **not** real Codex SQLite schemas, compressed rollouts, fork projections,
installation receipts, or a completed table/producer/consumer audit. Fixture labels
must not be interpreted as completed source-domain support.

The additional `fixtures/` SQL assets exercise the primary state, goals, logs,
both memory generations, queue, thread-history projection, and independent board
schemas at the pinned source commit. They cover **all eight** SQLite stores at the
schema/SQL level, with only partial current-primary producer/consumer mapping.
Stored public-protocol payloads remain synthetic. SQLx bookkeeping, Codex consumers, PostgreSQL, and
canonical history and artifacts are not qualified by these tests. See
`SOURCE_CATALOG.md` for the exact coverage and exclusions.
The matrix maps all 37 current fixture tables, including SQLite sequence state,
and eight file classes to implementation issues and direct source anchors. Its
`fixture_treatment` records only the offline fixture policy; it is not a final
migration decision. Producer/consumer closure and forward/reverse treatment are
explicitly unresolved. The file-class list has no independent completeness
check. A successful matrix audit never permits activation.
For the project/thread primary-state family, the matrix also records selected
direct read/write SQL clauses in `threads.rs` and `projects.rs`. The source test
checks those exact clauses still exist in production modules; it does not parse
SQL, enumerate every caller, or prove cross-host path handling. The transitive
producer/consumer audit stays partial.
`CANONICAL_HISTORY_SOURCES.md` separately pins a partial source-level map of
rollout, fork, archive, compression, and attachment call sites. It does not
qualify a coherent capture or second-host replay.
`SPLIT_STORE_FIXTURES.md` documents populated old primary logs/memory rows at
their drop boundaries alongside distinct or absent modern split stores.

`migrate`, `regenerate`, `retain`, and `absent` are distinct treatments. Only
`migrate` contributes verified record counts. Non-migrated entries cannot carry
chunks or claim copied records. The verifier does not execute regeneration or
prove that an `absent` declaration matches a real source filesystem.

## Limits and semantics

Raw and canonical rows are capped at 1 MiB, keys at 1,024 bytes, and cells at 256.
A manifest/inventory is capped at 1 MiB; domain count at 256; total chunk count at
4,096; individual chunks at 64 MiB. Input is consumed one row at a time; the verifier
does not retain the dataset. Encoding large individual values can temporarily use
more than one row's byte limit, but remains bounded by the fixed per-value limits.

Chunk boundaries must fall between newline-terminated records. Keys must increase
strictly across the entire domain, including between chunks. Raw chunk checksums,
chunk/domain counts and logical domain fingerprints are independently checked.
Trailing bytes, truncated records, unsupported versions and unknown fields fail closed.

There is no implicit JSON numeric coercion, Unicode normalization, timestamp
conversion, floating-point rounding, sorting, deduplication or payload truncation.
Adapters must explicitly normalize backend-specific booleans and timestamp units
according to their reviewed domain schema. Large artifacts need a reviewed chunked
representation; this code does not invent or silently split their identity.

## Remaining gates

The draft format needs independent architecture/security review and alignment with
the Rust storage interfaces before runtime adoption. Still required: the complete
source inventory, real legacy/current store fixtures, adapters and SQLx boundaries,
protected profiles, PostgreSQL-native migrations, portable artifact materialization,
writer fencing, coherent capture, resumable transfer, activation/recovery, current-data
reverse migration, exact-upstream qualification, server-owned API/CLI/TUI controls,
packaging, update persistence and safe uninstall.

This verifier must never substitute for source quiescence, authenticated capture
provenance, stale-plan revalidation, schema capability checks, public behavior tests,
or the release qualification of issues #4–#21. No PostgreSQL version or distribution
channel is claimed supported by this partial change.
