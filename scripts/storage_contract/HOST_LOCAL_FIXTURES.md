# Retained host records: a partial issue #2 fixture

Source pin: `CompleteDotTech/codex@395c622d693cf8ec1c4769cf71bc4a949da3e017`.
This fixture uses five exact SQL assets from `codex-rs/state/migrations/`.
`fixtures/host_local/PROVENANCE.json` records their Git blobs and SHA-256 digests.
The tests authenticate those blobs before executing the SQL. They do not
authenticate the complete 58-file tree or create a complete primary database.
The fixture normalizes Windows checkout CRLF to the pinned LF Git blobs before
hashing and executing SQL; this is test-only source normalization.
The base `threads` table is only a portable control, not the current thread schema.

## Source inventory and proposed treatment

| Records | Source-observed identity and behavior | Fixture treatment |
| --- | --- | --- |
| `remote_control_enrollments` | Composite URL/account/client key. Missing client name maps to an empty string. Optional enabled flag preserves NULL, false, and true. Timestamps use seconds. Enrollment upsert deliberately leaves the existing enabled flag unchanged; a separate setter changes it. | Retain on the source host. Do not project host enrollment authority onto another host. |
| `external_agent_config_imports` | `import_id` identity; nullable provider ID added in migration 0044; milliseconds; JSON success/failure arrays carry source/target/cwd and error metadata. Completion upserts all fields; history sorts by descending time then ascending import ID. | Retain host import bookkeeping rather than publishing it as portable session authority. |

These proposed treatments are confined to the fixture. A reviewed complete
inventory and production exclusion policy remain issue #2 acceptance gates.
Source producers and consumers read for this slice:

- [`runtime/remote_control.rs`](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/state/src/runtime/remote_control.rs): enrollment get/upsert/set-enabled/delete (lines 1–151 inspected).
- [`runtime/external_agent_config_imports.rs`](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/state/src/runtime/external_agent_config_imports.rs): completion writer, details reader, ordered history reader (complete production module inspected).
- [`runtime.rs`](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/state/src/runtime.rs): SQLite-owned runtime and initialization (lines 1–165 inspected).

## Verification boundary

The tests create disposable, closed SQLite files. A separately generated schema
policy comes from pinned SQL, never from the artifact being audited. The existing
`audit_snapshot` authenticates the whole file, stages a private copy, counts
retained rows, and fingerprints only the portable control rows. Retained values
must not enter typed-row encoding or appear in diagnostic output. Their bytes
still contribute to the whole-file digest and exist in the private staged copy;
this is not a secret-stripping exporter.

Coverage includes two distinct client keys, tri-state flags, nullable provider
identity, integers above 2^53, changed portable rows, changed retained values,
unknown/changed schemas, a missing table policy, and separate-process valid/stale
capture receipts. Source bytes must remain unchanged on success and failure.
A pre-0044 policy must reject the provider-ID schema, not silently drop it.

The fixture JSON follows the observed Rust record fields but does not prove Rust
serialization or app-server behavior. Windows/POSIX path strings are test data,
not foreign-host execution. This slice does not cover WAL capture, SQLx migration
metadata, all primary tables, filesystem artifacts, PostgreSQL, writer fencing,
activation, reverse migration, package upgrades, or upstream-binary compatibility.
Do not add this subset to the supported source catalog or mark issue #2 complete.

From the repository root, run only the focused Python suite:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts -p test_host_local_fixtures.py -v
```

Native Rust regressions and real-package qualification remain separate gates.
