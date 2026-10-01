# Goal persistence source edges — partial issue #2

The checked coverage matrix now records selected direct edges for both tables
in `goals_1.sqlite`. The test authenticates the pinned SQL fixture and checks
each named SQL or call-site clause in its specific production module. These
anchors are source evidence, not a complete call graph or a runtime test.
The test independently pins every nested module and operation key and checks
Rust anchors within the named method's lexical declaration range. Snapshot,
insert, replace, general update, active-status update and usage-accounting SQL
are separate entries; an `UPDATE` or `INSERT` in another method cannot satisfy
them. Active and idle usage callers are checked separately. The shared queue
and goal selector is a source-boundary check, not Rust parsing or execution.

| Record | Observed writer | Observed reader or consumer |
| --- | --- | --- |
| `thread_goals` | `GoalStore` inserts, replaces, updates usage/status, and deletes; `GoalService` handles external set/clear; app-server fork handling writes an inherited snapshot | `GoalStore::get_thread_goal`; goal runtime/tool/API and app-server goal get/notifications |
| `thread_goal_continuation_deferrals` | `GoalStore::replace_thread_goal_snapshot` inserts a deferral; goal extension clears it at turn start; deleting its goal cascades through the foreign key | Goal runtime checks deferral before starting idle continuation |

The app-server goal set path also appends a `thread_goal_updated_item` to the
canonical rollout. The SQLite goal row and rollout item therefore have distinct
writers and durability boundaries. This inventory does not prove they can be
captured atomically, replayed equivalently, or reconciled after partial failure.
Goal snapshot inheritance across forks, runtime accounting, stopped status,
feature-disabled behavior, and automatic continuation require issue #6's
native local/remote behavior tests.

The matrix retains `producer_consumer_audit: partial`, unresolved forward and
reverse treatment, and `activation_permitted: false`. App-server notification
consumers, every goal event, transitive callers and cross-store atomicity are
still unclosed. A source anchor check cannot qualify a PostgreSQL goal store.

Run the focused source check from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_coverage_matrix.py -v
```
