# Draft operation control plan and exclusion preview

`control_plan.py` validates a separate version-1 **offline** plan. The existing
version-1 export manifest remains unchanged and remains specific to migration.
This draft distinguishes three proposed operations without opening a database,
resolving credentials, mutating storage, or granting activation. A caller must
provide the expected plan SHA-256 and inventory independently of the plan.

The plan is bounded to 1 MiB and has exactly these fields: `version` (1),
`operation`, `operation_id`, `owner_host_id`, `source`, `destination`,
`inventory_sha256`, `manifest_sha256`, `destination_occupancy`, and
`local_history_present`. IDs use canonical UUID text. Source/destination
identities use the manifest's exact instance ID, dataset ID, generation and
backend shape. `local_history_present` and occupancy are **assertions**, not
validated observations. No endpoint, path, credential or package bytes are in
this plan.

| Operation | Source and export manifest | Destination assertion | Local history |
|---|---|---|---|
| `initialize_new` | Both absent | Empty PostgreSQL, generation 1 | Remains local; never imported |
| `migrate_local` | SQLite source and complete existing v1 manifest; operation ID, identities and digest must match | Empty PostgreSQL, next generation per manifest | Capture required under a future writer fence |
| `attach_existing` | Both absent | Existing PostgreSQL identity/generation | Remains local; never imported or merged |

The independently trusted inventory is bounded to 1 MiB. It must name the host
credential, device identity, remote-control enrollment, workspace checkout,
installation receipt, settings, journal and backup domains as `retain`.
Reclassifying or omitting one
fails this draft. A migration's existing manifest must exactly match all
inventory domains; non-migrated domains cannot carry payload. The preview lists
excluded domain IDs and their `retain`, `regenerate` or `absent` treatment.
For initialization and attachment, it additionally lists every portable
`migrate` domain under `not_imported_domains`, since those operations copy no
local data. It reports zero verified migrated records and
`activation_permitted: false`.
Domain IDs are policy labels, not discovered paths; the caller must ensure those
labels themselves contain no sensitive values.

This validator does not prove source-domain completeness, actual file/database
absence, target occupancy, host ownership, plan confirmation, current schema or
binary compatibility, live writer exclusion, remote permissions, credential
protection, transfer checkpoints, PostgreSQL behavior, or safe cutover. Those
facts require server-side preflight and revalidation at each irreversible step.
Reverse PostgreSQL-to-SQLite migration needs its own later control-plan version
or reviewed extension; this v1 plan only models local-to-remote migration.
The result does not close issue #2.

Run the synthetic contract tests from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_control_plan.py -v
```
