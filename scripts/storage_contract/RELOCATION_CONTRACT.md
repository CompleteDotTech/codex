# Offline host path relocation contract

`relocation.py` checks a bounded, version-1 path disposition plan against an
independently supplied plan digest, trusted source and target host IDs, dataset
ID, and observed source rows. The source rows must match the plan exactly. This
is a **planning preview**: it opens no source or target file, resolves no
symlinks, creates no directories, and never permits resume or activation.

The current slice covers three observed primary-state fields:

| Source field | Plan treatment |
| --- | --- |
| `threads.cwd` | Require one explicit source-root to target-root mapping; otherwise unresolved. |
| `project_roots.path` | Same mapping rule; the old root remains provenance only. |
| `threads.rollout_path` | Preserve a distinct portable rollout UUID; the source path is never a target identity or lookup location. |

Each path declares Windows or POSIX grammar, independent of the machine running
the verifier. Only absolute drive paths or absolute POSIX paths are accepted;
network paths, dot/dot-dot traversal, mixed POSIX separators, overlapping
matching roots, duplicate source records, and duplicate portable rollout IDs
are rejected. Root matching requires exact component casing, including the
drive letter; case-only differences remain unresolved. Windows source and
target components, including mapped suffixes, reject reserved characters,
control characters, device names, and trailing dots or spaces before joining.
These checks use conservative [Windows naming rules](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file).
A mapped candidate is an **unverified lexical relation**, not a
usable target path. Preview output contains only record IDs and dispositions,
never the source or proposed target path. Inputs themselves contain source
paths and must be protected accordingly.

The fixture authenticates all 58 pinned primary SQLite migration files, creates
an in-memory database, and reads synthetic Windows and POSIX thread and project
rows from source-shaped tables. Tests cover missing and ambiguous mappings,
traversal, wrong host identity, changed source rows, digest mismatch, and
path-free output, plus case-sensitive root matching and invalid Windows
components in source rows, mapping roots, and mapped POSIX suffixes. No user
database or actual workspace is opened.

Runtime owners must still authenticate the source capture and host affinity,
resolve the chosen target root and symlinks within a trusted workspace, verify
permissions and provider/tool equivalence, bind the canonical rollout object
to its portable ID, and reject unresolved paths before any resumed tool or
project action. Attachment references, arbitrary rollout item paths, model
payloads, case and Unicode equivalence, and cross-host execution remain
unqualified. This slice does not close issue #2.

Run the focused synthetic suite from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_relocation.py -v
```
