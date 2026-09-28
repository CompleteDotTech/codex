# Queue persistence source edges — partial issue #2

This checked inventory extends the current source coverage matrix for the two
tables in `queue_1.sqlite`. The test runs the independently pinned queue SQL
fixture and requires each listed SQL clause or caller anchor in its specific
native source file. It does not call Rust or enumerate every RPC entry point.

| Record | Observed producer | Observed consumer |
| --- | --- | --- |
| `queued_items` | `SqliteQueueStore` inserts, updates, deletes and reorders; `delete_thread_queue` removes a thread's items | `SqliteQueueStore::list_page`, through `LocalQueueStore` and `QueuedItemService`; app-server constructs the local adapter |
| `queued_thread_revisions` | SQLite triggers after every item insert, update and delete set a per-thread revision | `SqliteQueueStore::changes_since` reads revisions; `QueuedItemService` watches loaded thread IDs after `change_version` observes commits |

The source schema gives `queued_items` a unique `(thread_id, queue_order)`
index. The revision table's autoincrement high-water mark and SQLite
`PRAGMA data_version` are local notification mechanics. Preserving item order,
bounded enqueue behavior, and durable updates on another backend needs a
reviewed transaction and notification design in issue #7. The fixture's
`migrate` treatment is only an offline comparison rule; this stage does not
decide whether revision rows or generated-ID state are exported, regenerated,
or retained as source evidence. Forward and reverse treatment remain
unresolved in the matrix.

The matrix labels producer/consumer coverage `partial`: app-server request
handlers, queued turn admission, cancellation and cross-host notification paths
are not exhaustively traced here. This source audit does not prove snapshot
coherence, remote behavior, migration equivalence, or activation readiness.

Run the focused source check from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_coverage_matrix.py -v
```
