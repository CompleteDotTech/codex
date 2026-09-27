# Pinned source SQL catalog — developer qualification support

This is partial issue #2 work at fork base
`985cf47a4eb6084b2ff6b30ebdb1216acda85bb4`. It is not the PostgreSQL
backend, a production compatibility authority, a migration controller, or the
server-owned storage CLI. No active backend can be changed by these modules.

## Exact scope

| SQLite file | Source SQL included | Logical tables represented | Owning implementation issue |
|---|---|---|---|
| `state_5.sqlite` | **Not included** | Full primary/legacy inventory still outstanding | #5 and other domain issues |
| `goals_1.sqlite` | Goals migrations 1–2 | `thread_goals`, `thread_goal_continuation_deferrals` | #6 |
| `logs_2.sqlite` | Log migrations 1–2 | `logs`, generated-ID state | #8 |
| `memories_1.sqlite` | Memory migrations 1–2 | `stage1_outputs`, `jobs`, `consolidation_progress` | #6 |
| `memories_v2_1.sqlite` | Memory migrations 1–2, separate file | Same schema, separate records and leases | #6 |
| `queue_1.sqlite` | Queue migrations 1–2 | `queued_items`, `queued_thread_revisions`, generated-ID state | #7 |
| `thread_history_1.sqlite` | History migrations 1–7 | `thread_turns`, `thread_items`, `thread_history_projection_state`, `thread_realtime_items` | #11 |
| `agent_message_board_1.sqlite` | Embedded `SCHEMA` extraction | `channels`, `posts`, `subscriptions`, `subscription_opt_outs`, `deleted_boards`, generated-ID state | #9 |

SQLx's `_sqlx_migrations` table is **not** reproduced. A SQLx-created database
therefore must not be advertised as compatible with this fixture-only policy.
Runtime split-store capture, older primary-store versions, canonical JSONL and
compressed/fork histories, `session_index.jsonl`, memory files, attachment content,
host identities, installation receipts and every public consumer remain outside
this fixture catalog. Projection coverage is not canonical history coverage.

All inserted application records are synthetic. Their field fidelity is tested
at the SQL layer, not against Codex serialization or model-visible context.

## Source authentication and policy generation

`verified_migrations` authenticates every asset before returning any SQL. It
recomputes file Git blob IDs and the complete migration-directory Git tree ID.
Tree IDs were read from the pinned fork through GitHub. Omitting an asset or
rewriting its mutable checksum entry cannot replace that independently pinned
root. The board is an extracted Rust string, validated against its previously
verified SHA-256, not a claim to reconstruct the entire Rust source blob.

`build_fixture_policy` runs only that source SQL in a private in-memory database
and requires an explicit schema version. Unknown stores, out-of-range versions,
changed provenance and altered assets fail. An unknown primary store never
becomes a successful empty schema. Policies do not read or derive trust from the
artifact under examination. The existing artifact auditor still requires a
separately authenticated expected file digest.

The policy's `migrate` treatment means these fixture rows are included in the
logical fingerprint; it is not permission to transfer host-bound data or live
lease ownership. Successful audit reports always deny activation. Preserving
lease fields in a source backup is not reissuing them on a destination.

## Reproducible checks

Run from the package/repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts -v
```

The tests exercise old-to-new SQL backfills, complete-record preservation,
foreign-key cleanup, same-count corruption, nulls/large integers, both separate
memory files, source-policy/version rejection, generated IDs after supported
SQLite backup, and realtime cleanup. Fixtures exclusively create new files in
disposable directories; an existing destination is rejected before modification.

The old-writer history test deliberately demonstrates the source migration's
zero update-ordinal default. It is evidence that the old write remains possible,
not proof of fencing. The malformed-JSON test uses an explicitly managed SQLite
transaction; it is not SQLx/PostgreSQL crash-atomicity qualification.

No successful fixture result authorizes closing #2, activating remote storage,
restoring an unpatched binary, installing/removing a package, or publishing a
release. Those gates require the complete inventory, Rust/runtime integration,
independent review and the real process/platform/packaged-artifact journey.
