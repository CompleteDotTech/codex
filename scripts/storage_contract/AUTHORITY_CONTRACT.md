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
Inventory generated-ID state, including deleted-ID high-water marks and
SQLite/PostgreSQL sequences; copied rows alone do not prove safe next inserts.
It must also classify canonical active/archive/compressed/reference-backed
fork histories, session indexes, projections, memory outputs, referenced
artifacts, host identities, credentials, receipts, journals, and backups.
Absence or a disabled feature must be established by the capture, not assumed.

## 2. Identities are not paths or credentials

The following are proposed domain concepts, not existing public API fields:

| Concept | Required meaning |
| --- | --- |
| Dataset identity | Stable logical dataset across a verified move; not its endpoint or directory. |
| Storage-instance identity | Identity of one physical source or destination; changes when relocated or cloned. |
| Generation | Monotonic authority epoch; an expected-generation write must reject a stale epoch. |
| Namespace identity | Bound to the instance and effective authorization scope; schema name alone is insufficient. |
| Operation identity | Durable identity for one confirmed operation and its idempotent retries. |
| Owning-host identity | Storage-owning app-server host, distinct from terminal and exec-server hosts. |
| Artifact identity | Portable content identity plus length, format and provenance; not an absolute local path. |
| Package identity | Fork, exact upstream base, exact fork revision, target, channel and artifact digest. |

A matching dataset ID does not grant access. Authentication, trusted host-owned
configuration, namespace authorization and storage-administration permission
remain separate checks. Restoring a journal must not restore expired authority.
Remote profiles require certificate-verified encrypted transport and server
identity by default; reject silent TLS downgrade. Any trusted-local exception
must be narrowly scoped, explicit, independently reviewed and tested.

An existing SQLite home has no persisted storage identities. Before its first
capture or confirmed migration plan, adopt it under exclusive local writer
control: durably record one dataset ID, one local storage-instance ID and an
initial generation in a host-owned record outside replaceable package files.
Bind that record to an immutable home/incarnation marker and commit it
atomically before any staging or export can refer to those IDs. The current
content fingerprint is separate, refreshable capture evidence, not the
identity-recovery key; ordinary SQLite writes must not invalidate adoption.
Retry, restart, reinstall and recovery must recover and reuse the same record;
an incomplete or conflicting adoption must fail closed for reconciliation,
never mint a second identity for the same source history. Later authority
transitions advance the generation through the cutover protocol, not adoption.

## 3. Candidate configuration versus active authority

A saved profile is a candidate. Creating/editing/testing it must not change
active storage, create production tables, trigger import, or acknowledge a
backend switch. Untrusted repository configuration and model tools cannot
redirect private history or resolve protected credentials.

The active selection is durable server-owned state: dataset, instance,
namespace and generation, plus the host-local reference needed to connect.
Passwords and credential material are not embedded in this record, manifests,
receipts, logs, model context, snapshots or migration reports.

Persist an independently recoverable, home-bound activation marker before the
first remote cutover. If active-selection state is missing, corrupt or conflicts
with that marker, fail closed and reconcile authority before listing, resuming
or writing. Implicit SQLite bootstrap is allowed only for a home proven never
to have activated another authority; missing selection alone is not proof.

TUI, CLI, app-server, daemon, extensions and maintenance use one authority
service. On a remote outage they expose bounded unavailable/reconnecting
states; they must not create a writable local substitute. Local caches are
rebuildable and cannot become authoritative merely because they exist.
Read surfaces must also fail closed when authority or causal freshness cannot
be validated. Ephemeral threads remain memory-only, non-resumable and absent
from authority, migration, backups and exports; their lifecycle is explicit
even during a remote outage. Shared `memory/reset` requires dataset-wide
authorization and a defined shared versus host-local scope, or a versioned
rejection before any mutation.

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

Exports, staging, journals and retained backups contain private history. Use
least-privilege ownership and access, authenticated encryption where material
can leave the trusted host or disk boundary, bounded retention, and verified
cleanup or secure disposal after success, failure or cancellation. Preserve
recovery evidence for its declared retention window without leaving abandoned
plaintext staging indefinitely.

The current offline format caps rows at 1 MiB, keys at 1,024 bytes, cells at 256,
domains at 256, total chunks at 4,096 and chunks at 64 MiB. A larger source record
needs a separately reviewed chunked representation preserving identity and
ordering, not a raised hidden cap or silent loss. No existing format limit proves
that all runtime histories fit. Source paths are provenance/remapping inputs,
never portable event keys. Do not normalize Unicode, timestamps or numeric types
without a reviewed, reversible domain rule.

The existing experimental `thread/resume.path` and `thread/fork.path` surfaces
need an explicit
compatibility decision before migration: retain a host-local mapping from old
canonical path to migrated thread identity, or introduce a versioned/deprecated
transition that rejects those paths with usable replacements. Never open the
preserved stale files as current authority. Test both selected behaviors.
With remote authority active, `thread/resume.history` must either import
caller-supplied history through an authorized, explicitly scoped persistence
operation or return a versioned compatibility error before creating a thread.
It must never silently write outside the active authority.

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

Authority-dependent reads, including history lists and session resume/model
context assembly, must validate the current dataset, instance and generation
before serving data. A host with a cached retired selection must refresh or
fail closed; alternatively, keep the retired source unreadable to attached
hosts until they refresh. Reject stale reads before they can drive tool calls.
Within one generation, reads that assemble authoritative history must include
all acknowledged preceding writes. Read from the primary or use a verified
causal token/equivalent monotonic-read fence; replica or cache lag must never
silently omit completed turns from resumed model input.
Pagination cursors for authority-dependent APIs must be bound to the dataset
and generation, or proven compatible across cutover. A stale cursor returns a
defined restart error, never a page with silently skipped or duplicated items.
Peer app-server hosts need distributed notifications or bounded invalidation
and refresh for shared mutations, including delete, archive and rename.
Every live thread is bound to the dataset, instance and generation that supplied
its context. Drain it before authority activation or revalidate the binding
before each inference and external tool dispatch; stale in-memory turns cannot
continue merely because a later write would be rejected. Same-generation
writers on one thread need serialized turn/queue ownership or a defined conflict
that preserves exactly-once acknowledged items and model-input order.

Keep source and destination fenced through verification and authority commit.
Persist intent, decision and recovery evidence before allowing writes at the new
generation. A crash or lost response can produce an uncertain outcome, not an
assumed abort. Recovery must reconcile actual durable authority before writing.
Cancellation during commit reports the reconciled outcome; it cannot promise
that the old source is still current. Retain protected source backups.
Restoring or cloning a backup must reconcile against independently durable
latest-authority evidence before serving reads or writes. Give a new physical
copy a new instance identity, and advance the epoch through a verified recovery
transition or reject a rolled-back generation; a backup's old marker cannot
declare itself current merely because a host is offline.
Epoch advancement cannot make missing post-backup history current. Recover and
compare all acknowledged later data first; otherwise fail closed or create an
explicitly labeled fork with a new dataset identity.

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
| Reverse export for upstream | Fresh current-data export; exact selected unpatched binary can list/resume history and complete a mocked turn in isolation, with the resumed outbound model input compared against representative current histories, including fork-only and compaction items. Reject a target that silently drops them. |
| Foreign host combination | Real app/exec/client process tests for each advertised operating-system combination. |

An existing thread's persisted working directory is host-specific. Cross-host
resume requires verified host affinity or explicit workspace-root/path remapping
before project configuration or tools run; if neither is available, fail closed
with a remapping request. An absent `thread/resume.cwd` cannot inherit an
unverified source-host path.
Revalidate persisted permission profiles against trusted semantic-equivalence
on the destination host before tools run. A missing or same-named-but-different
profile requires explicit permission reselection; never silently fall back to
the destination host's broader default.
Apply the same equivalence or explicit-reselection gate to model-provider
endpoint and authentication definitions before sending history to a model.
Client-supplied dynamic tools require implementation-identity matching or
explicit re-registration before a cross-host resumed turn; missing or
conflicting tools disable the call or block the turn.

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
| Disable remote storage | Verified reverse migration of current remote data into a new isolated local instance by default; never activate stale backups or overwrite/merge preserved local stores. Preserve dataset identity only through a global cutover that durably retires the remote source for every client; a per-client local copy gets a new dataset identity and is labeled a fork. |
| Export a copy | Preserve remote authority and other clients; label the export as a copy, not a global cutover. |
| Local-client detach | Stop this client's participation; preserve the shared remote namespace and other clients. |
| Restore upstream | Verify current-data export with the exact unpatched target before changing executable resolution. |
| Uninstall fork software | Reconcile only owned artifacts/config/hooks, preserve user edits and data, retain recovery capability until verification. |
| Optional purge | Separate destructive operation with explicit scope and authorization; never implied by uninstall. |

Offline detach cannot attest to current remote contents. It must not represent
an old local backup as current history. Package-only removal launching upstream
requires an explicitly separate local home unless verified current-data restore
has completed. Report retained remote history accurately.

If unrelated local stores were preserved during initialization or attachment,
they remain a distinct dataset. Reverse migration may select an existing local
destination only through an explicit preservation protocol that identifies its
contents and ownership, rejects collisions, retains a recoverable copy, and
verifies the user's destination choice before activation. It must never silently
replace those stores or combine their history with remote data.

An intentional-uninstall decision is durable. Uninstall commits its tombstone
before package removal. Every patch-owned updater, including one staged before
uninstall, must serialize publication and activation with that tombstone under
one lock or compare-and-swap boundary; a stale updater loses the race and fails
activation. Remove only patch-owned hooks and verify a new-shell/reboot-equivalent
update probe does not reinstall the patch.
A deliberate reinstall is a new confirmed lifecycle operation and may explicitly
attach to retained remote data; it must not restart old import or updater work.

## 10. Required executable cases in dependent PRs

#3 must test candidate edits, managed policy, secret redaction and foreign-host
ownership. Adversarial repository configuration and model-tool attempts must
not redirect active authority or resolve protected credentials. #4 must test
exact supported versions, concurrent schema creation,
namespace isolation, cancellations, bounded pool waits and commit ambiguity.
Test a candidate connection against empty and foreign namespaces and prove it
creates no production schema, import or backend-switch acknowledgement.
#5–#9 must exercise shared SQLite/PostgreSQL public behavior and contention.

#2–#4 must exercise initialization with preserved local history and an
idempotent retry that reuses the same new dataset, plus attachment that neither
imports/merges local history nor advances the remote generation.
For initialize-new, cancel after disclosure of excluded local history and
verify no authority change; confirm the same disclosure and verify the exact
new dataset becomes active only afterward.
Test legacy adoption interruption immediately before and after record publication, restart
and reinstall recovery, and incomplete/conflicting records; no retry may mint a
second identity or orphan operation-owned staging. Interrupt and restore an
active remote connection and prove each write surface stays unavailable or
reconnecting without acquiring local authority.
Throughout outage and recovery, list, resume and model-input reads must also
fail closed or prove current causal authority. Start/fork ephemeral threads
while remote is active and unavailable; prove their history is absent from
durable listing, restart, migration and export, then cleaned up in memory.
Reject attachment before activation for mismatched dataset, namespace,
generation, authorization scope and reader/writer capability; validate the
remote transport identity and reject downgrade or an invalid certificate.

#10 must pause an old/stale writer through lease expiry and prove rejection;
include unobservable SQLite writers and opposing host clock skew against
database-consistent expiry. Also prove that a second host holding a
retired selection cannot list, resume or build model input from stale history,
and that lost/corrupt active selection never reactivates preserved SQLite data.
Pause a live turn across initialize, attach and cutover; it must be drained or
revalidated before another inference or tool call. Race two authorized hosts
on the same thread and queue; require serialization or a defined conflict,
exactly-once acknowledged items and identical resumed ordering. With skewed
host clocks, concurrently create/update threads and prove database-consistent
recency allocation and stable tie-breaking in lists and cursors.
Restart a journaled controller after lease or approval expiry and require fresh
acquisition or reconciliation before mutation. Keep an unrelated live session
beside an unobservable writer; migration must report a blocker without killing
that session, WSL or another user's process.
Race two confirmed controllers for the same generation: ownership acquisition
must be linearizable, exactly one destination may publish, and the loser must
discover and reconcile the durable winner before serving data.
#11–#15 must cover equal-count corruption,
missing references, late queue commits, repeated/uncertain append, every durable
cutover boundary, new remote writes before reverse migration and a second host
with no source files. No acknowledged canonical data may disappear or duplicate.
Pause a related operation between two store captures; the operation must drain,
retry or block rather than publish a mixed logical epoch. Cancel immediately
before and after the durable authority decision; both response and restart must
identify the actual winner. Reject omitted, unknown and duplicate/conflicting
inventory treatments, and unknown control-plan or export-extension versions,
before staging or mutation.
Capture a committed, uncheckpointed SQLite WAL and prove its rows reach the
destination. Alter a payload and all its internal hashes while retaining an
independently pinned expected digest; reject it before staging or cutover.
For every `regenerate` domain, omit it from the copy, rebuild it from verified
authority and compare its public consumer behavior with the source.
Compare representative pre-cutover SQLite outbound model input to post-cutover
PostgreSQL input, including fork-only and compaction items. Exercise exact-limit
and oversized/chunked rows, keys, cells and chunks; preserve Unicode, timestamp,
numeric and floating-point values exactly under reviewed domain rules. Include
deleted high IDs and sparse IDs, then insert after migration in every generated-ID
domain to prove high-water marks and ordering survive.
Exercise a reverse move with another remote writer: either every host observes
the retired source before a same-dataset local activation, or a per-client copy
uses a new dataset identity. Test preserved local destination collisions.
Compare representative pre/post reverse-cutover outbound model input for
PostgreSQL-to-SQLite history, including fork-only and compaction items.
Compare resumed outbound model input before and after each in-place schema
upgrade for histories written by old and new binaries, including fork-only and
compaction items.
After plan confirmation, change each bound source, destination occupancy,
authorization, schema capability and operation scope before cutover; every
irreversible transition must revalidate and stop on a changed binding.
Also substitute the exact destination instance, namespace and profile reference
while keeping the destination empty; reject redirection before staging. Try
migration, export and purge with initially read-only or ordinary-writer
principals; reject before mutation or private-data disclosure.

#16–#17 must exercise the same authorized service through JSON-RPC, CLI and
reviewed TUI snapshots, showing active authority separately from candidates.
Resume from caller-supplied history while remote is active and assert its
authorized persistence or versioned rejection. Request a second page after
cutover using a cursor from the old generation; continue compatibly or return
the defined stale-cursor restart error. Mutate a thread on one app-server host
and verify the subscribed peer refreshes or receives a notification.
#19–#21 must test actual packages, user-edited receipts/config, locked files,
concurrent updater/uninstall, offline detach, exact upstream restore and deliberate
reinstall. Exercise updater pause before publication, uninstall tombstone commit,
then updater resume; activation must fail. Test serialized and interrupted schema
upgrades, mixed old/new writers, and exact old-binary rollback against the
post-upgrade schema, rejecting incompatible binaries. For upstream restore,
compare resumed outbound model input for fork-only and compaction history, not
just list/resume success. Test backup restore/clone against a newer epoch, a
lagging same-generation replica and failover, staging cleanup/retention and
secret-bearing export access. #18 must run the
complete combined journey, not substitute tool fixtures. Exercise
`thread/resume.path` and `thread/fork.path` compatibility, and resume an
existing path-bound rollout from a second host/OS with no mapped workspace to
prove it fails closed. Test same-named conflicting and absent permission
profiles on that host before any tool executes.
Compare same-named conflicting and absent model-provider definitions before
any model request, and missing or conflicting dynamic-tool implementations
before tool dispatch.
Detach one of two live clients and prove the peer keeps listing, resuming and
writing against unchanged remote
authority. Export a copy with a live peer and verify remote dataset/generation
and peer read/write behavior remain unchanged. Cancel optional purge, uninstall
without purge, then authorize a narrowly scoped purge with out-of-scope data;
only the approved data may be removed. Restore a backup missing acknowledged
later turns and require recovery, a labeled fork, or fail-closed behavior.
Export private history off-host and prove no plaintext disclosure; wrong keys
and authentication-tag tampering must fail before import. Pause an updater after
file publication but before activation, then uninstall; either uninstall waits
for the entire update or stale activation loses. Remove the fork while remote
authority remains active without reverse migration; launching upstream must
use a separate local home or fail closed until current-data restore. Exercise
two-host `memory/reset` status and mutation with its declared scope and
authorization, including preservation of out-of-scope host-local data.

These are required future cases, not executed results. Independent architecture,
security, API and lifecycle review is outstanding. Issue #2 remains open until
its complete inventory, interface, manifest, fixture and compatibility criteria
are met; #1 and #18 remain open through full packaged qualification.
