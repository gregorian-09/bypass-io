use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::batch::{BatchError, ColumnData, RowBatch};
use crate::scan::{
    filter_scan_indices, range_filter_i64_mapped, ColumnPredicate, ScanError, ScanResult,
};
use crate::schema::Schema;
use crate::segment::{seal_batch, segment_ref, ImmutableSegment, Manifest};
use crate::wal::{WalError, WalReader, WalWriter};

/// A bypass-db table.
#[derive(Debug)]
pub struct Table {
    path: PathBuf,
    schema: Schema,
    active: RowBatch,
    sealed: Vec<ImmutableSegment>,
    manifest: Manifest,
    wal: WalWriter,
}

impl Table {
    /// Open or create a table at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when directories or WAL files cannot be created.
    pub fn open(path: impl AsRef<Path>, schema: Schema) -> Result<Self, TableError> {
        let path = path.as_ref().to_path_buf();
        fs::create_dir_all(path.join("WAL")).map_err(|err| TableError::Io(err.to_string()))?;
        fs::create_dir_all(path.join("segments")).map_err(|err| TableError::Io(err.to_string()))?;
        let wal_path = path.join("WAL").join("wal-000000.log");
        let manifest = Manifest::load(&path)?;
        let sealed = manifest
            .sealed_segments
            .iter()
            .map(|segment| ImmutableSegment::load(&path, segment))
            .collect::<Result<Vec<_>, _>>()?;
        manifest.store(&path)?;
        let wal = WalWriter::open(&wal_path)?;
        let mut active = RowBatch::empty(&schema);
        for batch in recover_batches_after(&schema, &wal_path, manifest.wal_records_applied)? {
            active.append(&batch)?;
        }
        Ok(Self {
            path,
            active,
            sealed,
            manifest,
            schema,
            wal,
        })
    }

    /// Table path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Table schema.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Number of active rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.active.row_count()
    }

    /// Number of sealed segments.
    #[must_use]
    pub fn sealed_segment_count(&self) -> usize {
        self.sealed.len()
    }

    /// Table manifest.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Append a row batch.
    ///
    /// Rows are serialized to the local WAL first, then appended to the active
    /// in-memory segment.
    ///
    /// # Errors
    ///
    /// Returns schema, serialization, or WAL errors.
    pub fn append(&mut self, rows: &RowBatch) -> Result<usize, TableError> {
        validate_batch_matches_schema(&self.schema, rows)?;
        let payload = encode_batch(rows)?;
        self.wal.append(&payload)?;
        self.active.append(rows)?;
        Ok(rows.row_count())
    }

    /// Scan rows where the timestamp is in `[start, end)`.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema timestamp column is missing from active
    /// data.
    pub fn scan_time_range(&self, start: i64, end: i64) -> Result<ScanResult, TableError> {
        let mut output = ScanResult::empty(&self.schema);
        let timestamp_column = self.schema.timestamp_column();
        for segment in &self.sealed {
            if !segment.overlaps_time_range(start, end) {
                continue;
            }
            let timestamp_map =
                Arc::new(segment.map_column(timestamp_column.name(), timestamp_column.dtype())?);
            let indices = range_filter_i64_mapped(&timestamp_map, start, end)?;
            if indices.is_empty() {
                continue;
            }
            let indices = Arc::<[usize]>::from(indices);
            let mut columns = Vec::with_capacity(self.schema.columns().len());
            for column in self.schema.columns() {
                let mapped = if column.name() == timestamp_column.name() {
                    Arc::clone(&timestamp_map)
                } else {
                    Arc::new(segment.map_column(column.name(), column.dtype())?)
                };
                columns.push((column.name().to_string(), mapped));
            }
            output.append_mapped_columns(columns, indices)?;
        }
        output.append_batch(&filter_time_batch(&self.schema, &self.active, start, end)?)?;
        Ok(output)
    }

    /// Scan a time range with an additional column predicate.
    ///
    /// # Errors
    ///
    /// Returns scan or table errors.
    pub fn scan_where(
        &self,
        time_range: (i64, i64),
        predicate: &dyn ColumnPredicate,
    ) -> Result<ScanResult, TableError> {
        let time_result = self.scan_time_range(time_range.0, time_range.1)?;
        let indices = filter_scan_indices(&time_result, predicate)?;
        Ok(time_result.take_indices(&indices))
    }

    /// Flush the WAL and seal active rows to an immutable segment.
    ///
    /// # Errors
    ///
    /// Returns a WAL, segment, or manifest error.
    pub fn flush(&mut self) -> Result<(), TableError> {
        self.wal.sync()?;
        if self.active.row_count() == 0 {
            return Ok(());
        }
        let segment_id = self.manifest.next_segment_id;
        let segment = seal_batch(&self.path, &self.schema, segment_id, &self.active)?;
        self.manifest.next_segment_id = self
            .manifest
            .next_segment_id
            .checked_add(1)
            .ok_or_else(|| TableError::Manifest("segment id overflow".to_string()))?;
        self.manifest.sealed_segments.push(segment_ref(&segment));
        self.manifest.wal_records_applied = WalReader::open(self.wal.path())?.records()?.len();
        self.manifest.store(&self.path)?;
        self.sealed.push(segment);
        self.active = RowBatch::empty(&self.schema);
        Ok(())
    }

    /// Recover raw WAL records for this table.
    ///
    /// # Errors
    ///
    /// Returns WAL errors.
    pub fn recover_records(&self) -> Result<Vec<Vec<u8>>, TableError> {
        let wal_path = self.path.join("WAL").join("wal-000000.log");
        Ok(WalReader::open(wal_path)?
            .records()?
            .into_iter()
            .map(|record| record.payload().to_vec())
            .collect())
    }

    /// Recover row batches from this table's WAL.
    ///
    /// # Errors
    ///
    /// Returns WAL or payload decoding errors.
    pub fn recover_batches(&self) -> Result<Vec<RowBatch>, TableError> {
        self.recover_records()?
            .iter()
            .map(|payload| decode_batch(&self.schema, payload))
            .collect()
    }

    /// Compact sealed segments into one replacement segment.
    ///
    /// Segment ids are compacted in manifest order. Active rows are not
    /// included; callers should call [`Table::flush`] first when they need all
    /// current rows sealed before compaction.
    ///
    /// # Errors
    ///
    /// Returns an error when an id is unknown, a segment cannot be read, the
    /// replacement segment cannot be written, or the manifest cannot be stored.
    pub fn compact(&mut self, segment_ids: &[u64]) -> Result<(), TableError> {
        if segment_ids.is_empty() {
            return Ok(());
        }

        let requested = segment_ids.iter().copied().collect::<BTreeSet<_>>();
        let present = self
            .sealed
            .iter()
            .filter_map(|segment| requested.contains(&segment.id()).then_some(segment.id()))
            .collect::<BTreeSet<_>>();
        let missing = requested.difference(&present).copied().collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(TableError::Segment(format!(
                "cannot compact unknown segment ids {missing:?}"
            )));
        }

        let mut combined = RowBatch::empty(&self.schema);
        let mut first_selected_index = None;
        let mut old_paths = Vec::new();
        for (idx, segment) in self.sealed.iter().enumerate() {
            if requested.contains(&segment.id()) {
                first_selected_index.get_or_insert(idx);
                combined.append(&segment.read_batch(&self.schema)?)?;
                old_paths.push(segment.path().to_path_buf());
            }
        }

        if combined.row_count() == 0 {
            return Ok(());
        }

        let replacement_id = self.manifest.next_segment_id;
        let replacement = seal_batch(&self.path, &self.schema, replacement_id, &combined)?;
        self.manifest.next_segment_id = self
            .manifest
            .next_segment_id
            .checked_add(1)
            .ok_or_else(|| TableError::Manifest("segment id overflow".to_string()))?;

        let insert_at = first_selected_index.expect("requested ids were present");
        let mut replacement_slot = Some(replacement);
        let mut new_sealed = Vec::with_capacity(self.sealed.len() - requested.len() + 1);
        for (idx, segment) in self.sealed.iter().cloned().enumerate() {
            if idx == insert_at {
                new_sealed.push(
                    replacement_slot
                        .take()
                        .expect("replacement inserted exactly once"),
                );
            }
            if !requested.contains(&segment.id()) {
                new_sealed.push(segment);
            }
        }

        self.manifest.sealed_segments = new_sealed.iter().map(segment_ref).collect();
        self.manifest.store(&self.path)?;
        self.sealed = new_sealed;

        for old_path in old_paths {
            if old_path.exists() {
                fs::remove_dir_all(old_path).map_err(|err| TableError::Io(err.to_string()))?;
            }
        }
        Ok(())
    }
}

fn recover_batches_after(
    schema: &Schema,
    wal_path: &Path,
    applied_records: usize,
) -> Result<Vec<RowBatch>, TableError> {
    WalReader::open(wal_path)?
        .records()?
        .into_iter()
        .skip(applied_records)
        .map(|record| decode_batch(schema, record.payload()))
        .collect()
}

fn filter_time_batch(
    schema: &Schema,
    batch: &RowBatch,
    start: i64,
    end: i64,
) -> Result<RowBatch, TableError> {
    let timestamp_column = schema.timestamp_column().name();
    let timestamps = batch
        .column(timestamp_column)
        .and_then(ColumnData::timestamp_values)
        .ok_or_else(|| TableError::MissingColumn {
            column: timestamp_column.to_string(),
        })?;
    let indices = timestamps
        .iter()
        .enumerate()
        .filter_map(|(idx, &ts)| (ts >= start && ts < end).then_some(idx))
        .collect::<Vec<_>>();
    Ok(batch.take_indices(&indices))
}

fn validate_batch_matches_schema(schema: &Schema, rows: &RowBatch) -> Result<(), TableError> {
    if schema.columns().len() != rows.columns().len() {
        return Err(TableError::Batch(BatchError::SchemaMismatch));
    }
    for column in schema.columns() {
        let data = rows
            .column(column.name())
            .ok_or_else(|| TableError::MissingColumn {
                column: column.name().to_string(),
            })?;
        if !data.matches_dtype(column.dtype()) {
            return Err(TableError::Batch(BatchError::ColumnTypeMismatch {
                column: column.name().to_string(),
                expected: column.dtype().clone(),
            }));
        }
    }
    Ok(())
}

fn encode_batch(rows: &RowBatch) -> Result<Vec<u8>, TableError> {
    let mut out = Vec::new();
    write_usize(&mut out, rows.row_count());
    write_usize(&mut out, rows.columns().len());
    for (name, data) in rows.columns() {
        write_bytes(&mut out, name.as_bytes())?;
        match data {
            ColumnData::F64(values) => {
                out.push(0);
                write_usize(&mut out, values.len());
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            ColumnData::I64(values) => {
                out.push(1);
                write_usize(&mut out, values.len());
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            ColumnData::Timestamp(values) => {
                out.push(2);
                write_usize(&mut out, values.len());
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            ColumnData::FixedStr { width, values } => {
                out.push(3);
                write_usize(&mut out, *width);
                write_usize(&mut out, values.len());
                for value in values {
                    write_bytes(&mut out, value)?;
                }
            }
        }
    }
    Ok(out)
}

fn decode_batch(schema: &Schema, payload: &[u8]) -> Result<RowBatch, TableError> {
    let mut input = PayloadReader::new(payload);
    let row_count = input.read_usize()?;
    let column_count = input.read_usize()?;
    if column_count != schema.columns().len() {
        return Err(TableError::PayloadDecode(
            "column count does not match schema",
        ));
    }

    let mut columns = Vec::with_capacity(column_count);
    for expected in schema.columns() {
        let name = String::from_utf8(input.read_bytes()?.to_vec())
            .map_err(|_| TableError::PayloadDecode("column name is not UTF-8"))?;
        if name != expected.name() {
            return Err(TableError::PayloadDecode(
                "column order does not match schema",
            ));
        }
        let tag = input.read_u8()?;
        let data = match tag {
            0 => {
                let count = input.read_usize()?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(f64::from_le_bytes(input.read_array()?));
                }
                ColumnData::F64(values)
            }
            1 => {
                let count = input.read_usize()?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(i64::from_le_bytes(input.read_array()?));
                }
                ColumnData::I64(values)
            }
            2 => {
                let count = input.read_usize()?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(i64::from_le_bytes(input.read_array()?));
                }
                ColumnData::Timestamp(values)
            }
            3 => {
                let width = input.read_usize()?;
                let count = input.read_usize()?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(input.read_bytes()?.to_vec());
                }
                ColumnData::FixedStr { width, values }
            }
            _ => return Err(TableError::PayloadDecode("unknown column tag")),
        };
        if !data.matches_dtype(expected.dtype()) {
            return Err(TableError::PayloadDecode(
                "decoded column type does not match schema",
            ));
        }
        columns.push((name, data));
    }
    input.finish()?;
    Ok(RowBatch::from_parts(columns, row_count))
}

fn write_usize(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u64).to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), TableError> {
    let len: u32 = bytes
        .len()
        .try_into()
        .map_err(|_| TableError::PayloadTooLarge { len: bytes.len() })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Table error.
#[derive(Clone, Debug, PartialEq)]
pub enum TableError {
    /// I/O error.
    Io(String),
    /// Batch validation error.
    Batch(BatchError),
    /// WAL error.
    Wal(WalError),
    /// Scan error.
    Scan(ScanError),
    /// Column was missing.
    MissingColumn {
        /// Missing column.
        column: String,
    },
    /// Payload is too large to encode.
    PayloadTooLarge {
        /// Payload length.
        len: usize,
    },
    /// WAL payload could not be decoded.
    PayloadDecode(&'static str),
    /// Segment read or write failed.
    Segment(String),
    /// Manifest read or write failed.
    Manifest(String),
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "table I/O error: {err}"),
            Self::Batch(err) => write!(f, "{err}"),
            Self::Wal(err) => write!(f, "{err}"),
            Self::Scan(err) => write!(f, "{err}"),
            Self::MissingColumn { column } => write!(f, "missing column {column}"),
            Self::PayloadTooLarge { len } => write!(f, "payload too large: {len} bytes"),
            Self::PayloadDecode(err) => write!(f, "payload decode error: {err}"),
            Self::Segment(err) => write!(f, "segment error: {err}"),
            Self::Manifest(err) => write!(f, "manifest error: {err}"),
        }
    }
}

impl Error for TableError {}

impl From<BatchError> for TableError {
    fn from(value: BatchError) -> Self {
        Self::Batch(value)
    }
}

impl From<WalError> for TableError {
    fn from(value: WalError) -> Self {
        Self::Wal(value)
    }
}

impl From<ScanError> for TableError {
    fn from(value: ScanError) -> Self {
        Self::Scan(value)
    }
}

struct PayloadReader<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> PayloadReader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, TableError> {
        let byte = *self
            .input
            .get(self.offset)
            .ok_or(TableError::PayloadDecode("unexpected end of payload"))?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_usize(&mut self) -> Result<usize, TableError> {
        let bytes = self.read_array::<8>()?;
        let value = u64::from_le_bytes(bytes);
        value
            .try_into()
            .map_err(|_| TableError::PayloadDecode("usize value overflow"))
    }

    fn read_bytes(&mut self) -> Result<&'a [u8], TableError> {
        let len_bytes = self.read_array::<4>()?;
        let len = u32::from_le_bytes(len_bytes) as usize;
        let end = self
            .offset
            .checked_add(len)
            .ok_or(TableError::PayloadDecode("payload offset overflow"))?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or(TableError::PayloadDecode("unexpected end of payload"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], TableError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(TableError::PayloadDecode("payload offset overflow"))?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or(TableError::PayloadDecode("unexpected end of payload"))?;
        self.offset = end;
        Ok(bytes.try_into().expect("slice length is fixed"))
    }

    fn finish(self) -> Result<(), TableError> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(TableError::PayloadDecode("trailing bytes after payload"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::batch::{ColumnData, RowBatch};
    use crate::scan::RangePredicate;
    use crate::schema::{ColumnDef, DType, Schema};

    use super::Table;

    static NEXT_TABLE: AtomicUsize = AtomicUsize::new(0);

    fn table_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "bypass-db-table-{name}-{}-{}",
            process::id(),
            NEXT_TABLE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn schema() -> Schema {
        Schema::new(
            "ticks",
            vec![
                ColumnDef::new("timestamp", DType::Timestamp).unwrap(),
                ColumnDef::new("price", DType::F64).unwrap(),
                ColumnDef::new("volume", DType::I64).unwrap(),
            ],
        )
        .unwrap()
    }

    fn batch(schema: &Schema) -> RowBatch {
        RowBatch::builder(schema)
            .column("timestamp", ColumnData::Timestamp(vec![10, 20, 30, 40]))
            .column("price", ColumnData::F64(vec![1.0, 2.0, 3.0, 4.0]))
            .column("volume", ColumnData::I64(vec![100, 200, 300, 400]))
            .build()
            .unwrap()
    }

    fn custom_batch(schema: &Schema, timestamp: i64, price: f64, volume: i64) -> RowBatch {
        RowBatch::builder(schema)
            .column("timestamp", ColumnData::Timestamp(vec![timestamp]))
            .column("price", ColumnData::F64(vec![price]))
            .column("volume", ColumnData::I64(vec![volume]))
            .build()
            .unwrap()
    }

    #[test]
    fn table_appends_wal_records_and_scans_time_range() {
        let path = table_path("scan");
        let schema = schema();
        let mut table = Table::open(&path, schema.clone()).unwrap();
        assert_eq!(table.append(&batch(&schema)).unwrap(), 4);
        table.flush().unwrap();

        let result = table.scan_time_range(15, 35).unwrap();
        assert_eq!(result.row_count(), 2);
        assert_eq!(
            result.column("price").unwrap().f64_values().unwrap(),
            vec![2.0, 3.0]
        );
        assert!(result.column("price").unwrap().has_mapped_chunks());
        assert_eq!(table.recover_records().unwrap().len(), 1);
        assert_eq!(table.recover_batches().unwrap(), vec![batch(&schema)]);

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn table_scan_where_combines_time_and_predicate_filters() {
        let path = table_path("where");
        let schema = schema();
        let mut table = Table::open(&path, schema.clone()).unwrap();
        table.append(&batch(&schema)).unwrap();

        let predicate = RangePredicate::new("price", 2.5, 5.0);
        let result = table.scan_where((0, 35), &predicate).unwrap();
        assert_eq!(result.row_count(), 1);
        assert_eq!(
            result.column("volume").unwrap().i64_values().unwrap(),
            vec![300]
        );

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn flush_seals_segment_and_reopen_scans_sealed_and_active_rows() {
        let path = table_path("sealed");
        let schema = schema();
        let mut table = Table::open(&path, schema.clone()).unwrap();
        table.append(&batch(&schema)).unwrap();
        table.flush().unwrap();

        assert_eq!(table.row_count(), 0);
        assert_eq!(table.sealed_segment_count(), 1);
        assert_eq!(table.manifest().sealed_segments.len(), 1);
        assert_eq!(table.manifest().wal_records_applied, 1);
        assert!(path.join("manifest.json").exists());
        assert!(path
            .join("segments")
            .join("seg-000001")
            .join("meta.json")
            .exists());
        assert!(path
            .join("segments")
            .join("seg-000001")
            .join("price.col")
            .exists());

        let result = table.scan_time_range(15, 45).unwrap();
        assert_eq!(result.row_count(), 3);
        assert_eq!(
            result.column("price").unwrap().f64_values().unwrap(),
            vec![2.0, 3.0, 4.0]
        );
        assert!(result.column("price").unwrap().has_mapped_chunks());

        table
            .append(
                &RowBatch::builder(&schema)
                    .column("timestamp", ColumnData::Timestamp(vec![50]))
                    .column("price", ColumnData::F64(vec![5.0]))
                    .column("volume", ColumnData::I64(vec![500]))
                    .build()
                    .unwrap(),
            )
            .unwrap();
        drop(table);

        let reopened = Table::open(&path, schema).unwrap();
        assert_eq!(reopened.sealed_segment_count(), 1);
        assert_eq!(reopened.row_count(), 1);
        let result = reopened.scan_time_range(35, 60).unwrap();
        assert_eq!(result.row_count(), 2);
        assert_eq!(
            result.column("price").unwrap().f64_values().unwrap(),
            vec![4.0, 5.0]
        );
        assert!(result.column("price").unwrap().has_mapped_chunks());

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn compact_replaces_selected_segments_and_preserves_scans_after_reopen() {
        let path = table_path("compact");
        let schema = schema();
        let mut table = Table::open(&path, schema.clone()).unwrap();

        table.append(&custom_batch(&schema, 10, 1.0, 100)).unwrap();
        table.flush().unwrap();
        table.append(&custom_batch(&schema, 20, 2.0, 200)).unwrap();
        table.flush().unwrap();

        assert_eq!(table.sealed_segment_count(), 2);
        assert!(path.join("segments").join("seg-000001").exists());
        assert!(path.join("segments").join("seg-000002").exists());

        table.compact(&[1, 2]).unwrap();
        assert_eq!(table.sealed_segment_count(), 1);
        assert_eq!(table.manifest().sealed_segments[0].id, 3);
        assert_eq!(table.manifest().next_segment_id, 4);
        assert!(!path.join("segments").join("seg-000001").exists());
        assert!(!path.join("segments").join("seg-000002").exists());
        assert!(path.join("segments").join("seg-000003").exists());

        let result = table.scan_time_range(0, 30).unwrap();
        assert_eq!(result.row_count(), 2);
        assert_eq!(
            result.column("price").unwrap().f64_values().unwrap(),
            vec![1.0, 2.0]
        );

        drop(table);
        let reopened = Table::open(&path, schema).unwrap();
        assert_eq!(reopened.sealed_segment_count(), 1);
        let result = reopened.scan_time_range(0, 30).unwrap();
        assert_eq!(result.row_count(), 2);
        assert_eq!(
            result.column("volume").unwrap().i64_values().unwrap(),
            vec![100, 200]
        );

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn compact_rejects_unknown_segment_ids() {
        let path = table_path("compact-missing");
        let schema = schema();
        let mut table = Table::open(&path, schema.clone()).unwrap();
        table.append(&custom_batch(&schema, 10, 1.0, 100)).unwrap();
        table.flush().unwrap();

        let err = table.compact(&[99]).unwrap_err();
        assert!(err.to_string().contains("unknown segment ids"));

        fs::remove_dir_all(path).unwrap();
    }
}
