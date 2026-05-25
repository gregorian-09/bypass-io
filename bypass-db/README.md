# bypass-db

`bypass-db` is the embedded columnar time-series layer for `bypass-io`.

This phase implements the stable Rust data model first: schemas, columnar row
batches, append-only table state, scalar scans, predicates, and a checksummed
write-ahead log format. The implementation is intentionally small and
self-contained so higher-performance mmap, SIMD, and native backend persistence
can be added on top of verified semantics.

