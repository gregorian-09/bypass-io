# bypass-db

`bypass-db` is the embedded columnar time-series layer for `bypass-io`.

The crate includes schemas, columnar row batches, append-only table state,
predicates, a checksummed write-ahead log format, immutable sealed segments,
mmap-backed scan chunks for sealed columns, segment compaction, and SIMD-assisted
range filtering where the current CPU supports the required vector extension.
