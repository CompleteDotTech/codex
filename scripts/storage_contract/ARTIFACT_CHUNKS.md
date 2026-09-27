# Draft bounded artifact-byte records

Partial issue #2 contract only. The SQLite default, Rust storage consumers,
app-server API, configuration, model context and installation are unchanged.
This is not a migration controller, production backend, or completed #11/#13.

## Trust and representation

The caller supplies an already captured, operator-owned binary stream and an
independently trusted `ArtifactSpec`: an opaque 32-byte identifier encoded as
64 lowercase hexadecimal characters, exact byte length, and SHA-256 digest.
The identifier is not a source path or necessarily a content hash. A future
inventory must bind it to its dataset, owners, format and required references.
This helper cannot establish that binding, freshness or source completeness.

The maximum artifact is 64 MiB, including zero bytes. Larger inputs are refused,
not truncated. Before emitting its first row, the encoder reads the source into
one private temporary file and authenticates its exact length and digest.
Encoder source reads and decoded fragments are at most 64 KiB. Short reads do not change fragment
boundaries. The caller still owns the input stream and must exhaust or close
the iterator. Temporary storage failure fails the operation; nothing activates.
Scratch bytes are not encrypted by this helper. The operator must protect the
scratch volume; abrupt-host-failure cleanup and secure erasure are not certified.

Each fragment uses the existing typed-row encoding from `FORMAT.md`. Its key is
32 artifact-ID bytes followed by the unsigned 8-byte big-endian byte offset.
The six ordered cells are:

| Position | Type | Value |
| --- | --- | --- |
| 0 | text | `CDTX-artifact-chunk-v1` |
| 1 | text | Independently supplied artifact ID |
| 2 | i64 | Complete artifact byte length |
| 3 | text | Complete artifact SHA-256 |
| 4 | i64 | Fragment byte offset |
| 5 | bytes | Exact fragment content, at most 65,536 decoded bytes |

Offsets start at zero and advance by 65,536; only the final fragment may be
shorter. A zero-byte artifact has one zero-length fragment at offset zero;
absence is a separate inventory decision. The marker and exact metadata are
checked on every row. Unknown versions or mismatched metadata fail closed.

The artifact auditor verifies complete ordered coverage and the complete content
digest. Missing, duplicate, reordered, truncated, extra and equal-length corrupted
content is rejected. It consumes bounded rows and writes no exported artifact.
It accepts the existing validator's canonical-equivalent row serialization;
transport byte identity still requires the enclosing manifest chunk checksums.
Payload bytes are never parsed, normalized, decoded as Unicode, or decompressed.
Preserving opaque bytes is not proof that a Codex parser accepts their contents.

## Relationship to the existing manifest

The v1 manifest field set is unchanged. A fragment-domain schema must explicitly
select this encoding; it is not a new default representation for existing domains.
Its record count counts **fragments**, not artifacts. A future domain adapter
must authenticate the complete independent artifact inventory, order artifact IDs
bytewise, reject duplicate IDs, and verify all references and per-artifact receipts.
The current helper verifies one artifact only; it does not do that enumeration.

Manifest transport chunks may group complete fragment rows within existing limits.
Base64 expansion means a 64 MiB artifact cannot fit in one 64 MiB transport chunk;
chunk at row boundaries and respect the existing total/chunk/record limits.
The unchanged `Fingerprint` and `verify` APIs are exercised by a test using
multiple transport chunks. The existing 1 MiB row cap is not raised.

## Evidence and remaining gates

Focused tests exercise short reads, arbitrary bytes, a synthetic JSON payload
larger than 16 MiB, full-source authentication before emission, zero length,
corruption, metadata binding, malformed streams, cancellation and an isolated
Python file/process boundary. The large JSON is **not** a native rollout fixture.
No Rust/SQLx/PostgreSQL, cross-host resume, process fencing, interrupted migration,
reverse export, live data or packaged binary is exercised by these tests.

Successful receipts always say `activation_permitted: false`. Durable capture,
resume/checkpoints, atomic verified output publication, artifact/path remapping,
compression/fork-coordinate validation, namespace authorization and full runtime
consumer tests remain dependent implementation work. No new user-facing command
or supported PostgreSQL/platform/version claim is introduced by this slice.
