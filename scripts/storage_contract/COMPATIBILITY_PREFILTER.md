# Draft offline compatibility prefilter

`compatibility.py` compares a bounded version-1 descriptor with a bounded caller
observation. The caller supplies the expected descriptor SHA-256 independently;
the evaluator refuses a descriptor whose digest differs. Both JSON objects have
strict fields, and the result uses fixed reason codes without echoing payloads.

The descriptor pins a fork revision, upstream base, package digest and target
identity. It lists supported reader and writer schema versions separately for
each domain, allowed backend/server-major tuples, protocol and daemon versions,
rollout and artifact formats, and required capabilities. The observation names
one operation (`read`, `write`, `update`, or `upstream_restore`), one version for
each domain, and an asserted migration phase. Every domain must be present;
`write` and `update` require writer support as well as reader support. An unknown
or active migration phase is refused.

This is a **planning prefilter**, not a compatibility certificate. A matching
result says `compatible_for_planning`, `activation_permitted: false`, and
`observations_verified: false`; it always names attested observation, native
package qualification, and runtime revalidation as remaining gates. The caller
observation is not gathered or attested by this module. It does not inspect a
binary, server, database, daemon, rollout, artifact, or installed package.
`update` checks the supplied candidate state but does not validate a transition
from a previous state. `upstream_restore` is always refused because the exact
unpatched target's native SQLite reader/writer qualification is missing.

The synthetic test descriptor includes a PostgreSQL major number solely to
exercise tuple comparison. It does not assert support for that version, or for
PostgreSQL at all. Actual package and server versions, domain completeness,
protocol/daemon handshakes, recovery behavior, path relocation and cutover
remain outside this stage. This prefilter does not close issue #2.

Run its tests from the repository root:

```sh
python -m unittest discover -s scripts/storage_contract -t scripts \
  -p test_compatibility.py -v
```
