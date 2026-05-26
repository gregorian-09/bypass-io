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

## Compaction

`Table::compact` merges selected sealed segment ids into one new sealed segment.
The selected segments are read in manifest order, appended into one replacement
batch, and written as a new `segments/seg-NNNNNN/` directory.

After the replacement segment is written, `manifest.json` is updated so the new
segment occupies the first selected segment's position. The old segment
directories are then removed. This preserves scan order while reducing the
number of sealed segment files a query must inspect.

Active rows are not included in compaction. Call `Table::flush` first when
current active rows should be sealed before compaction.

## Mmap Column Access

`ImmutableSegment::map_column` opens one sealed column file as a read-only
memory map and returns a `MappedColumn`. The mapping validates that:

- the column exists in `meta.json`
- the requested logical type matches the stored metadata
- the file byte length equals `row_count * type_width`

`MappedColumn` exposes raw mapped bytes plus scalar readers for `F64`, `I64`,
`Timestamp`, and `FixedStr(N)`. The scalar readers copy one value out of the
mapped bytes on demand. They intentionally do not expose typed slices such as
`&[f64]`, because file mappings are byte-addressed and safe typed slices require
additional alignment and aliasing proof.

## Current Boundary

`Table::scan_time_range` still materializes sealed rows into owned `RowBatch`
values before building `ScanResult`. The lower-level sealed segment API now has
mmap-backed column access, so a future scan phase can keep selected scan columns
backed by mappings instead of eagerly copying whole column files.
