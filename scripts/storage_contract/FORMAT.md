# Draft version 1 audit encoding

This format is not an accepted production migration protocol. It is the concrete
contract implemented by this review slice; see README.md for exclusions and gates.

## Inventory

UTF-8 JSON object with exactly `version` (integer 1) and `domains` (nonempty list).
Each rule has exactly `id`, `schema` (positive signed-64-bit integer) and `treatment`.
IDs match `[a-z][a-z0-9_.-]{0,127}`. Duplicate IDs are invalid.
Treatments are `migrate`, `regenerate`, `retain`, or `absent`.

## Manifest

The manifest has exactly these fields:

| Field | Meaning |
|---|---|
| `version` | Integer 1; booleans do not satisfy integer fields. |
| `operation` | `migrate`; initialization and attachment cannot use this format. |
| `migration_id` | Canonical lowercase UUID text. |
| `source` | Source identity object. |
| `destination` | Destination identity object. |
| `domains` | Complete list matching the independent inventory. |

Identity objects have exactly `instance_id`, `dataset_id`, `generation`, and
`backend`. IDs are canonical lowercase UUIDs; generation is a positive signed
64-bit integer. Backends are `sqlite` and `postgresql`. This draft requires opposite
backends, distinct storage-instance IDs, an unchanged portable dataset ID and a
destination generation exactly one greater than the captured source generation.
These fields do not themselves prove destination emptiness, freshness or ownership.

Domain entries have exactly `id`, `schema`, `treatment`, `count`, `logical_sha256`,
and `chunks`. Schema and treatment must match the trusted inventory. `count` is a
nonnegative signed-64-bit integer. A migrated domain has a logical digest and zero
or more chunks. An empty migrated domain has the fingerprint of zero records,
not a null/arbitrary digest. Non-migrated domains have count zero, null logical
digest and no chunks.

Each chunk has exactly `bytes`, `records` and `sha256`, with positive integer byte
and record counts, and a lowercase 64-hex-digit SHA-256 digest. Payload is the exact
concatenation of chunk bytes in manifest/domain order. No path is interpreted and
no external content is fetched. Checkpoint offsets can be derived from the ordered
chunk sizes; this verifier does not implement transfer checkpoint persistence or resume.

Manifest SHA-256 covers the exact original UTF-8 bytes. The caller supplies the
expected hash from a trusted capture/plan. Duplicate JSON fields, non-integer JSON
numbers, non-finite JSON numbers and unknown fields are rejected.

## Logical records

Each raw line is one object with exactly `key` and `values`. `key` is nonempty,
lowercase, even-length hexadecimal for opaque portable key bytes. `values` is an
ordered array of cells, each with exactly `type` and `value`.

| Cell type | Value encoding |
|---|---|
| `null` | JSON null. |
| `bool` | JSON true or false. |
| `i64` | Canonical decimal string in the signed 64-bit range; no leading zeros or `-0`. |
| `f64` | 16 lowercase hex digits representing exact big-endian IEEE-754 bits. |
| `text` | Unicode scalar text, validated as UTF-8; no normalization. |
| `bytes` | Canonical padded standard Base64; no whitespace or alternative pad bits. |

Canonical JSON uses recursively sorted object keys, ASCII escaping, no whitespace
outside strings, and one final LF. No floating-point conversion is used to validate
`f64` bit strings: signed zero, infinities and NaN payloads remain distinguishable.

The logical SHA-256 preimage is: ASCII `CDTX-logical-v1` followed by NUL; a 4-byte
big-endian unsigned domain-name length; the ASCII domain name; an 8-byte big-endian
schema number; for each canonical row, its 8-byte big-endian byte length followed
by its exact bytes including LF; and finally an 8-byte big-endian record count.
Keys must be strictly increasing bytewise, including across chunk boundaries.
The domain adapter, not the database's collation, defines key encoding.

Raw checksums prove transport-byte equality. Logical fingerprints prove equality of
the typed rows actually supplied under a domain/schema. Neither proves an exhaustive
source capture, real-world referential integrity, application behavior or authority.
