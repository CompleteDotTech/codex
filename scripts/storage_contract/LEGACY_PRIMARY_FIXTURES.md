# Populated legacy primary-store fixtures

This is partial issue #2 evidence, not current primary-store support or a storage
backend. The source catalog's seven-of-eight current-file coverage is unchanged.

`legacy_primary_test_support.py` uses the first nine primary SQL migrations at
fork `395c622d693cf8ec1c4769cf71bc4a949da3e017`, identified by their exact Git
blob IDs. It creates disposable populated schema prefixes 6–9. These numbers are
**migration prefixes, not SQLite filename generations or supported Codex releases**.
All nine trusted repository assets must match before any destination is created;
this helper does not authenticate the complete primary migration directory.
The fixture normalizes Windows checkout CRLF to the pinned LF Git blobs before
hashing and applying SQL; this is test-only source normalization.

## Evidence supplied

The fixtures populate threads, dynamic tools, logs, memory outputs, jobs, consumed
log IDs and backfill metadata. Tests cover first-user-message backfill, nullable
rollout slugs, full records after reopen, Unicode/NUL fidelity, large integers,
foreign-origin path strings, compound uniqueness and actual foreign-key cascades.
They demonstrate that thread deletion does not cascade into legacy logs or jobs.
Those rows therefore cannot be assumed absent merely because threads were removed.

The existing offline auditor detects inventory omissions, stale schemas, broken
references, changed bytes and equal-count payload changes. A disposable process
commits to WAL and exits without closing SQLite: the raw database is rejected;
SQLite's backup API captures the committed row, and another process audits the
standalone backup without modifying it or exposing payloads in its report.

Retain/regenerate variants test explicit audit reporting only. They do **not**
authorize excluding jobs from migration, transferring live leases, choosing a new
backend, or replacing production inventory with fixture-derived policy.

## Deliberate exclusions

No current `state_5.sqlite`/complete migration-chain policy, SQLx migration ledger,
legacy-to-split-store transfer, real Codex payload deserialization, canonical
rollouts, required artifact content, host/device classification, PostgreSQL,
writer fencing, install/update/reverse/uninstall or exact upstream-binary
compatibility is established here. Foreign path strings are not cross-OS tests.
A successful offline report always has `activation_permitted: false`.

Run from a complete checkout with Python >=3.10:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts -p 'test_legacy_primary*.py' -v
```

Run the complete existing storage-contract suite and repository formatter/checks
before publication; native platform and independent review gates remain separate.
