# Rollout snapshot preview (partial issue #2)

Run `python scripts/preview_rollout_snapshot.py --snapshot-home HOME` against a
user-supplied snapshot of a Codex home. The command reads canonical rollout
names under `sessions/YYYY/MM/DD/` and `archived_sessions/`. It reports physical
plain and compressed copies per rollout ID, including same-directory siblings
and active/archive duplicates. It reads at most the first 64 KiB of each plain
file for `SessionMeta.history_base` and reports direct missing or ambiguous
ancestor IDs. Compressed headers remain unknown; it never decompresses them.

The JSON preview contains counts and at most 32 examples per category. Examples
contain rollout IDs only; source paths and rollout payloads are omitted. The
walk stops after 100,000 entries or 10,000 directories; symlinks are not
followed. A rejected input or exceeded bound produces no partial result.

The input path alone cannot prove an immutable snapshot. The preview always
sets `capture_complete=false` and `activation_permitted=false`. It cannot prove
lineage closure for compressed-only children, validate whole-file bytes,
resolve conflicting copies, fence concurrent writers/compression/moves, or
authorize export, import, resume, or storage activation. A future capture must
prove external fencing or snapshot consistency and establish a provenance rule
before choosing any copy. It must also inventory non-rollout file classes and
SQLite stores from the source catalog.
