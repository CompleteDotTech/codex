# Storage authority and lifecycle contract — proposed issue #2 slice

Status: **unreviewed design candidate; not a runtime implementation**.
Base: `395c622d693cf8ec1c4769cf71bc4a949da3e017`.
Related issues: #2, #3, #4, #10, #13–#21; parent #1.

This document specifies acceptance obligations for dependent implementations.
It does not enable PostgreSQL, change the SQLite default, add a command, or
approve a migration or package. A checked format, source fingerprint, or SQL
fixture is not evidence that a Codex runtime consumer works remotely.

## 1. Source observations and remaining inventory work

At the pinned base, `codex-rs/state/src/sqlite.rs` declares seven runtime stores:
`state_5.sqlite`, `logs_2.sqlite`, `goals_1.sqlite`, `memories_1.sqlite`,
`memories_v2_1.sqlite`, `queue_1.sqlite`, and `thread_history_1.sqlite`.
Its Git blob is `f05caf1f7e36ead0e316ddf890415cf95a6b9e34`.
The separate board store is catalogued as `agent_message_board_1.sqlite` in
`SOURCE_CATALOG.md` (blob `fcfa1929ebf2dc91a8b2dfaf0362422d189c7331`).

The existing catalog explicitly omits the primary-state fixture and SQLx
bookkeeping. Its seven-store SQL coverage is not eight-store runtime coverage.
A complete table/column/producer/consumer inventory is still required; this
slice must not be used to tick that acceptance criterion.

The final inventory must map each domain to its current and legacy source,
authority, producer, every reader, owning issue, target representation,
forward/reverse treatment, key/order encoding, and verification procedure.
It must also classify canonical active/archive/compressed/reference-backed
fork histories, session indexes, projections, memory outputs, referenced
artifacts, host identities, credentials, receipts, journals, and backups.
Absence or a disabled feature must be established by the capture, not assumed.

## 2. Identities are not paths or credentials

The following are proposed domain concepts, not existing public API fields:

| Concept | Required meaning |
| --- | --- |
| Dataset identity | Stable logical dataset across a verified move; not its endpoint or directory. |
| Storage-instance identity | Identity of one physical source or destination; changes when relocated. |
| Generation | Monotonic authority epoch; an expected-generation write must reject a stale epoch. |
| Namespace identity | Bound to the instance and effective authorization scope; schema name alone is insufficient. |
| Operation identity | Durable identity for one confirmed operation and its idempotent retries. |
| Owning-host identity | Storage-owning app-server host, distinct from terminal and exec-server hosts. |
| Artifact identity | Portable content identity plus length, format and provenance; not an absolute local path. |
| Package identity | Fork, exact upstream base, exact fork revision, target, channel and artifact digest. |

A matching dataset ID does not grant access. Authentication, trusted host-owned
configuration, namespace authorization and storage-administration permission
remain separate checks. Restoring a journal must not restore expired authority.

## 3. Candidate configuration versus active authority

A saved profile is a candidate. Creating/editing/testing it must not change
active storage, create production tables, trigger import, or acknowledge a
backend switch. Untrusted repository configuration and model tools cannot
redirect private history or resolve protected credentials.

The active selection is durable server-owned state: dataset, instance,
namespace and generation, plus the host-local reference needed to connect.
Passwords and credential material are not embedded in this record, manifests,
receipts, logs, model context, snapshots or migration reports.

TUI, CLI, app-server, daemon, extensions and maintenance use one authority
service. On a remote outage they expose bounded unavailable/reconnecting
states; they must not create a writable local substitute. Local caches are
rebuildable and cannot become authoritative merely because they exist.

## 4. Three distinct entry operations

| Operation | Source handling | Destination handling | Authority result |
| --- | --- | --- | --- |
| Initialize-new | Existing local history remains untouched and is not imported. | Create an explicitly new, isolated dataset after identity/occupancy checks. | Activate only through a confirmed selection protocol that discloses excluded local history. |
| Migrate-local | Capture every inventoried domain under enforceable writer exclusion. | Populate a dedicated operation-owned staging destination; verify complete coverage. | Switch only through the separately verified cutover protocol. |
| Attach-existing-remote | Preserve unrelated local stores without uploading or merging them. | Read and validate the exact remote dataset, namespace, schema and capabilities. | Join the existing authoritative generation; do not invent a new dataset generation merely for another client. |

Initialization is not migration. Attachment is not merge. A populated foreign
namespace is never treated as an empty destination. Retrying initialization
must identify the same operation-owned result or fail, not create another one.

## 5. Confirmed plan and immutable export are different contracts

`manifest.py` at this base accepts an exact version-1 field set and only the
`migrate` operation. It checks opposite backends, distinct storage instances,
matching dataset identity and destination generation equal to source plus one.
That verifier always returns `activation_permitted: false`.

Do not add fields to the existing v1 document and claim backward compatibility.
Use a separately versioned control-plan document; version an export extension
explicitly when its semantics change. Unknown versions must fail closed.

A confirmed control plan must bind the operation kind/ID, owner host, exact
source/destination identities, active generation, schema/capability versions,
source capture, inventory revision, target occupancy, retained backup and
policy decisions. Revalidate these bindings after ownership acquisition and
immediately before every irreversible transition. A digest is meaningful only
when supplied or confirmed independently of an untrusted export.

Plan expiry is not writer fencing. A cached successful connection test is not
current permission or compatibility. No approval survives a changed destination,
changed source, changed required package/schema, or changed operation scope.

## 6. Completeness, exclusions and bounded representations

For an independently trusted inventory I, require exactly one explicit treatment
for every domain in I and reject unknown or duplicate domains. For every migrated
domain, compare identities, typed values, counts, ordering, relationships and
canonical fingerprints, then exercise representative public consumers.
Equal counts alone cannot establish correctness.

`migrate`, `regenerate`, `retain` and `absent` are distinct. Retained credentials,
device keys, enrollment records, unrelated checkouts, installed tools and live
process state must not count as successfully migrated content. Rebuildable data
requires a demonstrated rebuild from verified authority; its source cannot be
omitted merely because the destination has an empty projection.

Classify user-authored/consolidated memory and extension/ad-hoc files separately
from generated memory outputs. Required portable artifact bytes must be available
without the source host. Missing/corrupt content blocks cutover pending explicit
resolution; it cannot be silently truncated or replaced with a dangling path.

The current offline format caps rows at 1 MiB, keys at 1,024 bytes, cells at 256,
domains at 256, total chunks at 4,096 and chunks at 64 MiB. A larger source record
needs a separately reviewed chunked representation preserving identity and
ordering, not a raised hidden cap or silent loss. No existing format limit proves
that all runtime histories fit. Source paths are provenance/remapping inputs,
never portable event keys. Do not normalize Unicode, timestamps or numeric types
without a reviewed, reversible domain rule.

## 7. Enforceable write ownership and cutover

A coherent capture covers all independent databases, canonical files, artifact
writers, boards, logs, background jobs and maintenance. Eight individually valid
SQLite backups taken at unrelated times are not a coherent dataset snapshot.
Use supported SQLite capture mechanisms that include committed WAL state.

A registration list of new clients cannot fence old/unobservable writers.
Establish enforceable exclusive-access/offline preconditions or stop with a
blocker. Do not terminate WSL, unrelated sessions or user processes to create
that precondition. Drain/release only resources actually owned by the operation.

Normal thread writes and store-wide migration require durable leases/fencing.
A paused writer whose lease expired must fail on its next write even after
reconnect. Verify clock-skew behavior and database-consistent time semantics.
Do not transfer a live source lease as valid destination ownership.

Keep source and destination fenced through verification and authority commit.
Persist intent, decision and recovery evidence before allowing writes at the new
generation. A crash or lost response can produce an uncertain outcome, not an
assumed abort. Recovery must reconcile actual durable authority before writing.
Cancellation during commit reports the reconciled outcome; it cannot promise
that the old source is still current. Retain protected source backups.

## 8. Binary, schema and host compatibility matrix

The release gate must bind exact upstream base, fork revision/patch identity,
package digest/target/channel, reader/writer schema range, protocol capabilities,
app-server and daemon identity, storage-owning host, exec host and client host.
Numeric version equality, a fork suffix or a retained executable alone is not
proof of compatibility. Resolve and test the actual executable, not only PATH.

| Transition | Evidence required before mutation/activation |
| --- | --- |
| SQLite default start | Existing config and runtime behavior remain compatible. |
| PostgreSQL opt-in | Explicit supported server versions, native schema/pool tests, consumer parity and completed feature qualification. |
| New client attaches | Correct dataset/namespace/generation; authorized compatible protocol and reader/writer capabilities. |
| Mixed/older client writes | Enforced compatible writer capability or a rejection before mutation. |
| Schema upgrade | Serialized migration plus compatibility/recovery checks before running the migration, not just before replacing a binary. |
| Fork update or reinstall | Verified complete fork-owned runtime bundle; preserved backend identity, settings, journals and ownership receipt. |
| Binary rollback | Exact old binary is a safe reader/writer of the current schema; otherwise block. |
| Reverse export for upstream | Fresh current-data export; exact selected unpatched binary can list/resume history and complete a mocked turn in isolation. |
| Foreign host combination | Real app/exec/client process tests for each advertised operating-system combination. |

No PostgreSQL major, package channel or host combination is certified by this
slice. The reviewed #4 server-version policy and #18 qualification receipts must
populate the release matrix before opt-in. Missing evidence means unsupported,
not a wildcard. A new upstream commit invalidates assumptions only where its
source/capabilities affect the qualified tuple; it is never silently certified.

## 9. Installation and removal are distinct from data authority

Receipts and operation journals live outside replaceable package directories.
Record exact owned files/config changes, prior resolution, content hashes,
activation state, backups, identity and updater ownership without secrets.
Unknown package-manager ownership or conflicting user changes require a report,
not broad deletion. Preserve shared credentials and external services by default.

| Action | Required retained state and authority effect |
| --- | --- |
| Disable remote storage | Verified reverse migration of current remote data; never activate stale backups. |
| Export a copy | Preserve remote authority and other clients; label the export as a copy, not a global cutover. |
| Local-client detach | Stop this client's participation; preserve the shared remote namespace and other clients. |
| Restore upstream | Verify current-data export with the exact unpatched target before changing executable resolution. |
| Uninstall fork software | Reconcile only owned artifacts/config/hooks, preserve user edits and data, retain recovery capability until verification. |
| Optional purge | Separate destructive operation with explicit scope and authorization; never implied by uninstall. |

Offline detach cannot attest to current remote contents. It must not represent
an old local backup as current history. Package-only removal launching upstream
requires an explicitly separate local home unless verified current-data restore
has completed. Report retained remote history accurately.

An intentional-uninstall decision is durable. Every patch-owned updater must
check lifecycle ownership and that decision at its publication/activation point,
including an updater staged before uninstall. Remove only patch-owned hooks and
verify a new-shell/reboot-equivalent/update probe does not reinstall the patch.
A deliberate reinstall is a new confirmed lifecycle operation and may explicitly
attach to retained remote data; it must not restart old import or updater work.

## 10. Required executable cases in dependent PRs

#3 must test candidate edits, managed policy, secret redaction and foreign-host
ownership. #4 must test exact supported versions, concurrent schema creation,
namespace isolation, cancellations, bounded pool waits and commit ambiguity.
#5–#9 must exercise shared SQLite/PostgreSQL public behavior and contention.

#10 must pause an old/stale writer through lease expiry and prove rejection;
include unobservable SQLite writers. #11–#15 must cover equal-count corruption,
missing references, late queue commits, repeated/uncertain append, every durable
cutover boundary, new remote writes before reverse migration and a second host
with no source files. No acknowledged canonical data may disappear or duplicate.

#16–#17 must exercise the same authorized service through JSON-RPC, CLI and
reviewed TUI snapshots, showing active authority separately from candidates.
#19–#21 must test actual packages, user-edited receipts/config, locked files,
concurrent updater/uninstall, offline detach, exact upstream restore and deliberate
reinstall. #18 must run the complete combined journey, not substitute tool fixtures.

These are required future cases, not executed results. Independent architecture,
security, API and lifecycle review is outstanding. Issue #2 remains open until
its complete inventory, interface, manifest, fixture and compatibility criteria
are met; #1 and #18 remain open through full packaged qualification.
