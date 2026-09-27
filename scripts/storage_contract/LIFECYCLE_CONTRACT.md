# Storage lifecycle compatibility and authority contract

Status: **proposed developer-contract supplement for issue #2**. This document
requires architecture review. It does not add an executable feature, qualify a
release, change the existing audit format, or satisfy the complete issue gate.
Implementation belongs to #3-#21 in the epic's dependency order. No runtime
backend, migration, installer, updater, or uninstall command is enabled here.
Read this package/install supplement with [AUTHORITY_CONTRACT.md](AUTHORITY_CONTRACT.md).
That document defines dataset authority and cutover behavior; this one adds
package identity, installation ownership, update, and removal requirements.

## Source pin and observed boundaries

The source observations were made at `395c622d693cf8ec1c4769cf71bc4a949da3e017`.
Their listed blobs remain unchanged at integration base
`061d352d3f3854290eefe41f1674022f8470ebde` (which includes #57).
These observations are about the retrieved source ranges, not an exhaustive
runtime inventory. Links are immutable; re-audit them after any upstream sync.

| Source at the pinned base | Observed boundary | Consequence for this contract |
| --- | --- | --- |
| [install-context/src/lib.rs, lines 1-230](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/install-context/src/lib.rs#L1-L230) | `CodexPackageManifest` exposes a semantic `version`; `InstallMethod` distinguishes standalone, npm, bun, pnpm, Vite+, Brew, and other installations. | Version and installation method alone do not establish fork capability or data compatibility. Preserve current layouts while adding separate ownership/capability evidence. |
| [tui/src/update_action.rs, lines 1-200](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/tui/src/update_action.rs#L1-L200) | Update commands select upstream npm-family packages, the Brew cask, upstream standalone installers, or a daemon source. | A fork-owned install must not use those commands unchanged as proof of patch-preserving updates. Each supported route needs a fork-aware decision or a safe refusal. |
| [app-server-daemon/src/managed_install.rs, lines 1-180](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/codex-rs/app-server-daemon/src/managed_install.rs#L1-L180) | Daemon package selection handles dedicated and legacy roots. Public updater eligibility checks release naming, `auto-update-version`, and canonical paths; older binaries are probed for update-loop support. | Keep legacy detection and process ownership separate from patch ownership. Neither a matching path nor a successful help probe proves PostgreSQL/schema support. |
| [SOURCE_CATALOG.md](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/scripts/storage_contract/SOURCE_CATALOG.md) | The merged SQL fixture catalog excludes primary state, SQLx migration metadata, canonical histories/artifacts, lifecycle receipts, and complete public consumers. | Do not turn successful fixture audits into permission to activate, restore upstream, or close #2/#18. |
| [FORMAT.md](https://github.com/CompleteDotTech/codex/blob/395c622d693cf8ec1c4769cf71bc4a949da3e017/scripts/storage_contract/FORMAT.md) | Draft audit v1 accepts only `migrate`; domain records have typed hashes. Source/destination identities do not prove ownership or freshness. | Initialization/attachment need distinct operations. A future production envelope must be versioned independently; do not silently extend strict v1 parsers or reinterpret their hashes. |

Verified source blob identifiers, respectively:
`1a9be86a1169e701e07d07152a9dd9a9886be000`,
`860d0bd10203f542c0cbf66f7715180f16a73e98`,
`ce3107fbd53c028400998973928c0b409b92b0a9`,
`fcfa1929ebf2dc91a8b2dfaf0362422d189c7331`, and
`16bb555b8fd5eba5c3236a431f61d4698e55f455`.

Requirements are anchored in [#2](https://github.com/CompleteDotTech/codex/issues/2),
[#10](https://github.com/CompleteDotTech/codex/issues/10),
[#14](https://github.com/CompleteDotTech/codex/issues/14),
[#15](https://github.com/CompleteDotTech/codex/issues/15), and
[#19](https://github.com/CompleteDotTech/codex/issues/19),
[#20](https://github.com/CompleteDotTech/codex/issues/20), and
[#21](https://github.com/CompleteDotTech/codex/issues/21).
The definitions below are proposed design decisions, not assertions that matching
fields, storage directories, or APIs already exist in the source.

## Authority and identity

One storage-owning app-server controls an operation. Its host may differ from
both the terminal host and the exec-server host. Every status, plan, confirmation,
and result identifies that host without exposing a machine credential.

Keep four identities separate:

- Package identity: fork owner, patch identity/version, upstream base commit,
  source commit, target OS/architecture, executable/helper/resource digests,
  channel, and authenticated release-manifest identity.
- Installation identity: locally generated installation ID, user/security
  principal, owned activation targets and hooks, and the durable receipt format.
- Dataset identity: portable dataset ID, backend instance and namespace IDs,
  authority generation, domain schema vector, and current migration ID if any.
- Client identity: host/client enrollment and current lease/fencing epoch. Never
  copy a source host's enrollment secrets or live lease into another host.

A receipt or manifest is not trusted merely because it parses or has a checksum.
Release evidence must chain to an independently trusted publisher. Local receipts
must be protected by the storage-owning user's permissions and checked against
current files, resolved targets, and process identity before privileged effects.
Configuration, environment overrides, repository files, and model/tool content
must not grant lifecycle administration privileges.

The active-backend record, candidate profile, migration journal, ownership receipt,
and updater policy are separate durable objects. They live outside replaceable
package directories. Their final paths and native durability primitives require
review and platform tests; this document intentionally does not reserve a path.

## Lifecycle persistence inventory supplement

This is the lifecycle slice, not the missing complete table/file inventory.
`Retain` means not counted as transferred portable application data.

| Durable domain | Producer / reader boundary | Migration and relocation treatment | Owning issues |
| --- | --- | --- | --- |
| Package manifest and capability identity | Package builder; installer; startup and daemon compatibility probes | Install verified target-native files; do not migrate source executable paths as portable references. | #19, #20 |
| Activation receipt and previous target | Installer; updater; recovery; uninstall | Retain on owning host outside package trees. Restore only unchanged, receipt-owned targets. Record user edits as conflicts. | #19-#21 |
| Active backend / namespace / generation | Server-owned storage controller; every persistence consumer | Retain per host, bound to the remote dataset when attached. Change only after verified activation. A foreign host explicitly enrolls/attaches. | #3, #12, #14, #16 |
| Candidate profile and credential reference | Trusted host configuration; preflight service | Retain profile locally. Re-resolve or re-enroll protected credentials on a new host; never upload credential values as data. | #3, #16, #17 |
| Migration intent, checkpoints, commit evidence | Storage-owning migration controller; startup recovery | Retain durable local recovery records and agreed remote control records. Authenticate plan identities and phase evidence before resumption. | #10, #13-#16 |
| Source and reverse-export backups | Supported snapshot/export path; recovery and exact-version verifier | Retain immutable provenance and hashes. A backup is not current authority merely because a newer backend is unavailable. | #13-#15, #21 |
| Update channel, selection, and suppression state | Installer; CLI/TUI update dispatch; daemon updater | Retain per installation. Intentional uninstall records suppression before cleanup; deliberate reinstall requires explicit fresh consent. | #19-#21 |
| Hook/service/launcher ownership | Installer; platform service manager; uninstall | Retain exact identifiers and expected content/targets. Never transfer a live PID, service identity, or scheduled task to another machine. | #19-#21 |
| User settings and local edits | User/configuration layers; trusted configuration writer | Preserve non-owned keys, comments where supported, and files. Patch-owned edits use conflict-aware restoration, not whole-file backup overwrite. | #3, #19-#21 |
| Device keys, provider/account secrets, workspace checkouts, installed tools | Existing host-local owners | Explicit exclusion in preview. Retain or separately re-enroll; do not claim these bytes as migrated application data. | #2, #3, #11-#13 |

Do not conflate the Compose helper's resource receipt with an installed Codex patch
receipt. A database/container ownership check is neither a Codex installation
record nor a migration authority decision.

## Distinct operations and consent boundaries

| Operation | Authority transition | Required behavior |
| --- | --- | --- |
| Initialize new remote | Explicitly selected new remote dataset | Require destination identity and emptiness checks. Do not import local history. Keep existing local data intact. |
| Migrate local | Current local dataset to a verified remote generation | Bind the plan to both identities, inventory, snapshots, and writer fence. Transfer all required portable domains before activation. |
| Attach existing remote | This client's selected backend changes to a known remote dataset | Verify identity/schema/permissions. Never merge or upload local history implicitly. Preserve local files as inactive data. |
| Reverse migrate | Current remote generation to fresh local SQLite | Include writes since original cutover; verify every required domain and portable artifact. Preserve dataset identity only through a fenced global move that retires the remote source for every client; otherwise label the local copy with a new fork dataset identity. Never reactivate an old source backup as current data. |
| Disable remote storage | Verified transition back to local, not a preference toggle | Use the reverse-migration gate. With no verified local destination, stop and present recovery/attach options instead of local fallback. |
| Detach this client | This client's connection/ownership ends | Preserve shared remote data and other clients. Do not advertise retained local history as current. Provide an explicit inactive/error state. |
| Update or reinstall patch | Package generation changes; dataset authority normally does not | Recheck capability/schema/daemon compatibility and receipts. Preserve active selection, user edits, credentials, and recovery state. |
| Restore exact upstream | Verified upstream package replaces the fork entrypoint | Require the selected upstream executable to pass current-data compatibility tests before activation. Preserve the fork recovery path. |
| Uninstall software | Receipt-owned software/hooks are removed or deactivated | Suppress patch-owned updates first; preserve data, backups, edits, and shared credentials. Uninstall is not data purge. |
| Deliberate reinstall | New explicit activation after uninstall | Verify the package again and require explicit initialization, migration, or attachment. Do not let a stale updater trigger this operation. |

Any destructive data purge is a separate, explicitly authorized operation outside
this feature's default uninstall flow. A software uninstall must neither require
nor silently perform deletion of a shared PostgreSQL dataset.

## Compatibility descriptor and decision rules

Define a versioned descriptor before production interfaces are implemented.
Its symbolic fields are design terms, not a wire/schema change in this supplement.

The descriptor includes `descriptor_version`, fork/patch and upstream identities,
package digest and target, per-domain reader/writer schema support, rollout and
artifact format versions, protocol/daemon compatibility, required capability IDs,
and supported backend/server combinations. Do not encode all these axes as one
semantic package version. Reader support never implies writer support.

A runtime decision also needs the actual active dataset, authoritative schema and
control generation, operation kind, authenticated package/installation evidence,
and current migration phase. Do not rely on a package's self-reported version or
on a stale status snapshot to authorize an update.

For each operation:

1. Authenticate the exact package/receipt and the storage-owning administrator.
2. Re-read backend, namespace, schema vector, control epoch, and migration state.
3. Require understood descriptor/journal versions and every required capability.
4. Check each affected domain's reader and writer compatibility, including
   tombstones, sequence/ordering rules, and canonical-history/artifact formats.
5. Check the actual client/server/daemon/exec platform combination. A successful
   build or `--help` probe is not an interoperability test.
6. Refuse write/activation on unknown, missing, stale, or incompatible evidence.
   A diagnostic-only result must not create writable SQLite or mutate selection.
7. Revalidate the fenced identities and expected generation at the commit point.

Matching version strings are insufficient. Matching schema numbers are insufficient
when patch capabilities, payload interpretation, or writer fencing differ. A
compatible old reader may be read-only only where that behavior is implemented and
qualified; it must never obtain a write lease by implication.

## Install and upgrade qualification matrix

These are required qualification rows, **not a list of tested/supported releases**.
No complete fork-owned PostgreSQL package tuple is qualified by this supplement.
Every production row must later name exact package digests and evidence receipts.

| Scenario | Required package/data/host evidence | Safe result without evidence | Gate |
| --- | --- | --- | --- |
| First patched install over a local SQLite installation | Inventory; exact old/new executable identities; owned activation targets; compatible local schema | Leave prior entrypoint and data unchanged | #2, #19 |
| Same patched package reinstall | Matching receipt and current files; current backend/journal; conflict-aware config merge | Refuse destructive repair; report conflicts | #19, #20 |
| Patched version A to B with remote active | Authenticated fork channel; reader/writer/protocol/daemon matrix; all required native assets | Do not activate B; keep A and remote selection | #4, #20 |
| Upstream rebase of the fork | Exact old/new upstream bases; patch capability regression checks; schema and context compatibility | No supported-update claim | #20, #18 |
| Daemon update from this CLI | Both executable identities; server-owned status; daemon protocol/capabilities and migration phase | Leave running compatible daemon intact or explicit recovery state | #16, #20 |
| Public/latest updater encountered by a patched install | Fork-preserving package decision or explicit unsupported-route policy | Stop before upstream activation | #20 |
| Second host, including foreign client/exec OS | Native package assets; explicit attach; namespace verification; relocated paths; no source files | No implicit local initialization or merge | #11, #12, #18 |
| Upgrade while migration is prepared/transferring/verifying | Exact journal-reader compatibility and safe recovery implementation | Defer update; retain journal/source/active authority | #14, #20 |
| Rollback after new package wrote new schema | Exact prior binary compatibility with current data, or verified explicit migration to fresh compatible destination | Refuse rollback, not restore stale backups | #15, #20 |
| Remote to fresh SQLite then exact upstream restore | Current remote snapshot; complete reverse export; exact upstream binary and native helpers; process-boundary tests | Keep fork recovery and current authority intact | #15, #21 |
| Uninstall followed by restart/update tick | Durable suppression; verified hook/service ownership; pending updater fencing | Do not reinstall or reactivate the patch | #21, #18 |
| Intentional reinstall and attach after uninstall | Fresh consent, verified package, explicit dataset identity, no stale hook authority | Leave detached/inactive state unchanged | #19, #21, #18 |

Initial engines are SQLite and PostgreSQL only. PostgreSQL version selection must
be recorded in #2/#4's reviewed backend matrix with exact qualified versions and
settings. A standalone helper selecting PostgreSQL 17 does not qualify Codex on
that major version. No additional server major version or installer channel is
made supported by this document. Unsupported combinations fail before activation.

## Crash-safe authority and update semantics

A confirmed plan binds operation ID, source/destination instance and dataset IDs,
namespace, expected source generation, destination generation, inventory/version,
package compatibility descriptor, snapshot identity, and retained backup identity.
Changing an identity, exclusion, schema, or package target invalidates confirmation.

Writer fencing must cover each authoritative store, rollout writer, background
worker, message board, and attached host. Lease checks use server-authoritative
expiry/epochs where applicable; stale holders cannot commit after losing a lease.
An advisory lock or observed quiet period alone does not fence an old unmanaged
client. Where old-client exclusion cannot be established, block cutover or require
a documented, verifiable isolation precondition; do not invent fencing proof.

Persist intent before effects, authenticate checkpoints, and make replay idempotent
by operation and domain identity. Distinguish an absent write from an unknown
commit outcome. After a lost response, reconcile durable operation evidence before
retrying; never assume a timeout proves rollback.

Activation requires complete capture, destination identity/conflict checks,
per-domain verification, canonical references and artifacts, required reconstruction,
and the final fenced generation check. Publish one authoritative selection. A
crash at any phase must recover from durable evidence to that single authority or
an explicit blocked recovery state. Candidate configuration is never authority.

Keep migration and installation journals separate but cross-reference their
operation/package identities. A software update during unresolved storage recovery
may proceed only if it has qualified journal/descriptor compatibility and does not
remove the executable/resources needed for recovery. Otherwise defer it.

A protected local uninstall intent is written before patch-owned hooks are
removed. Updaters must revalidate its generation under the same installation
coordination boundary immediately before activation. Losing that boundary cancels
activation. Uninstall must coordinate or stop only positively identified,
installation-owned updater processes; never terminate WSL or unrelated sessions.

Native atomic replacement, permission enforcement, directory durability and service
control differ by platform. Use platform-specific implementations and failure
injection; do not call a POSIX-only rename demonstration Windows qualification.
Keep prior owned executables and source backups until policy explicitly permits
cleanup. On conflicts or uncertain effects, retain evidence and stop destructively
modifying the installation.

## Exact upstream restoration qualification

Reverse export captures the current remote dataset under the appropriate fence,
including acknowledged writes after initial migration. Build a fresh destination;
never mutate or promote the old local backup into the exported current dataset.

Verify counts, typed hashes, referential closure, tombstones, precision, ordering,
canonical rollouts/forks, projections, artifacts and source exclusions. Include
SQLx migration bookkeeping where required by the selected upstream target; the
fixture catalog's omission is not permission to omit it in a production export.
Compare resumed outbound model input for representative current histories,
including fork-only, compaction, and injected response items, so a target that
silently drops an unknown variant fails qualification.

In a disposable copy, launch the exact authenticated unpatched target package,
with its native helpers and a test home. Exercise real public resume/fork/search
and relevant domain read/write/reopen paths. Check both acknowledged results and
resulting persistent state. Prove new writes can be reopened by that same binary.
A version print, successful launch, SQLite integrity check, or fixture fingerprint
alone does not meet this gate. The test must not access live provider credentials.

Bind the result to the exported generation, inventory/format versions, exact
upstream package digest, platform, and test evidence. Re-check authority freshness
before real activation. If the dataset has advanced, repeat export/verification or
apply the reviewed fenced delta procedure; an old passing receipt is insufficient.

## Review and validation obligations

Required behavioral cases are specifications, **not tests executed here**:

- Unknown descriptor/journal/schema or missing fork capability refuses activation.
- Same version text with different package digests or upstream bases is rejected
  unless the exact new tuple has independently authenticated compatibility proof.
- Read-compatible but write-incompatible clients cannot acquire write authority.
- Candidate profile save/test leaves active selection and data untouched.
- Old-client writes, expired leases, delayed commits, and simultaneous migrations
  cannot create two authorities; inability to fence results in explicit refusal.
- Crash after intent, transfer, verification, commit and local selection write
  recovers idempotently, including lost acknowledgement of a committed operation.
- New remote writes survive reverse export and the exact upstream package test.
- Foreign terminal/app-server/exec host combinations use the storage owner's data,
  credentials, package target and filesystem rather than the caller's paths.
- Remote outage, rejected update, and detach do not create writable local fallback.
- User-edited launchers/settings and shared credentials survive update/uninstall;
  changed symlink targets, forged receipts, and unrelated processes are untouched.
- Concurrent updater/uninstall, restart after uninstall, and deliberate reinstall
  obey suppression, generation fencing, and fresh explicit attachment consent.

Before implementation, review this supplement with #3/#4/#10/#14-#16/#19-#21 owners
and agree the versioned Rust interfaces in focused crates. Schema/API generation,
Bazel assets/locks, native integration tests and reviewed TUI snapshots belong with
those executable changes. No new persistence code belongs in codex-core by default.

This supplement leaves #2's full source-faithful domain/producer/consumer inventory,
primary/legacy and disabled-feature fixtures, complete portable manifest, Rust
storage interfaces, host relocation rules for every artifact type, and final
backend/package support matrix **open**. It does not close #2, #56, #18, or #1.
