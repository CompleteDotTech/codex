# Versioned memory source edges — partial issue #2

The matrix records selected SQL and native call edges for the same three tables
in `memories_1.sqlite` and `memories_v2_1.sqlite`. The fixture authenticates the
pinned two-file memory schema for each store; source checks bind each edge to a
specific module. The stores have distinct rows and leases, even though they
share SQL definitions. V2 is opened lazily, while an existing V2 file is
checked at startup. Absence is not an empty V2 database.

| Domain | Observed producer | Observed consumer or boundary |
| --- | --- | --- |
| `stage1_outputs` | `MemoryStore` stage-1 success upsert, usage update, retention/delete | Memory selection and phase-2 input preparation; `storage.rs` materializes raw memory and rollout summary files from selected rows |
| `jobs` | `MemoryStore` inserts claims and updates leases/status | Stage-1 and phase-2 worker selection; lease ownership is process/host state, not transferable worker authority |
| `consolidation_progress` | `MemoryStore` raises/resets max thread count | `memory_readiness.rs` reads the singleton readiness marker |
| Generated memory files | `storage.rs` writes raw-memory and rollout-summary files; phase 2 also writes consolidated/extension content through other paths | Memory read and prompt paths require separate enumeration and semantic checks |

`MemoryStore` also reads the primary thread catalog through its separate
`state_pool`; a capture of either memory database alone is not a coherent
thread-plus-memory snapshot. Phase-2 file output is not transactionally tied to
the SQLite rows. The raw-memory and rollout-summary outputs may be rebuildable
from verified rows, but this stage does not classify the consolidated
`MEMORY.md`, extension/ad-hoc files, workspace diffs, or every consumer as
rebuildable. Treating the entire `memory_artifact` class that way would lose
authoritative content. V1/V2 rollback, in-flight leases, and cross-store
reset/deletion need issue #6 and later qualification.

The matrix retains partial producer/consumer coverage, unresolved forward and
reverse treatment, and `activation_permitted: false`. Source anchors do not
establish migration completeness or remote behavior.

Run the focused source check from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_coverage_matrix.py -v
```
