# Synthetic legacy split-store transition fixtures

This is partial issue #2 source evidence. `test_split_store_fixtures.py`
creates disposable SQLite files from the authenticated SQL assets in
`source_catalog.py`; its primary data begins with the pinned populated prefix-9
fixture. Prefix numbers are SQL migration ordinals, not supported Codex versions.

The fixture covers two destructive source boundaries:

| Primary prefix before | Next source migration | Existing old rows | Separate modern file |
|---|---|---|---|
| 22 | `0023_drop_logs.sql` | `logs` row and consumed ID | `logs_2.sqlite` has a distinct row |
| 34 | `0035_drop_memory_tables.sql` | `stage1_outputs` and leased `jobs` rows | `memories_1.sqlite` has distinct rows |

At each boundary, the test inventories the primary and separate locations
independently, applies only the next primary migration, and checks that the old
primary rows disappear while the separate rows remain unchanged. This SQL does
not copy the old rows into the new file. A separate fixture establishes that a
missing optional file and an existing empty file have different inventory
states. The synthetic old memory job deliberately retains worker and lease
fields as source data, without granting their ownership on another host.

An exporter must identify a source version and include every present location
before a destructive upgrade, or prove that earlier rows were already moved by
an independently qualified procedure. A later empty modern store cannot prove
that old primary data was absent. No transfer algorithm, merge rule, or remote
writer ownership is approved by these tests.

The fixture has no SQLx `_sqlx_migrations` history; it does not run a Codex
binary, capture a live WAL, cover all historical prefixes, qualify real payload
serialization or a complete domain inventory, or exercise PostgreSQL. It does
not prove that the current runtime upgrades old installations in a particular
way. No test result permits activation or closes issue #2.

Run from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_split_store_fixtures.py -v
```
