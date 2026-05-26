# bypass-db Storage Layout

`bypass-db` stores table data under a table root path:

```text
<table>/
  WAL/
    wal-000000.log
  segments/
    seg-000001/
      meta.json
      timestamp.col
      price.col
      volume.col
  manifest.json
```

## WAL

Appends are written to the WAL before active in-memory rows are updated. The
current WAL file is `WAL/wal-000000.log`.

`manifest.json` stores `wal_records_applied`, the number of WAL records already
represented by sealed segment files. On open, the table replays only WAL records
after that boundary into the active segment.

## Sealed Segments

`Table::flush` syncs the WAL, writes the active rows into a new immutable
segment directory, updates `manifest.json`, and clears the in-memory active
rows.

Each `.col` file is a flat little-endian array in schema order:

- `F64`: one 8-byte little-endian `f64` per row.
- `I64`: one 8-byte little-endian `i64` per row.
- `Timestamp`: one 8-byte little-endian `i64` per row.
- `FixedStr(N)`: exactly `N` bytes per row.

`meta.json` records the segment id, row count, timestamp min/max, and ordered
column file metadata.

## Manifest

`manifest.json` records:

- manifest format version
- active WAL path
- WAL records already sealed into segments
- next segment id
- ordered sealed segment references

The manifest is the table's durable index of sealed data. Segment directories
not listed in the manifest are not part of the table.

## Current Boundary

This phase reads sealed column files back into owned `RowBatch` values. It does
not use `mmap` yet. The stable file format and manifest semantics come first;
mmap-backed scan columns can be added once this layout is exercised by more
tests and benchmarks.
