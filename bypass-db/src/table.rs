use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::batch::{BatchError, ColumnData, RowBatch};
use crate::scan::{filter_indices, ColumnPredicate, ScanError, ScanResult};
use crate::schema::Schema;
use crate::wal::{WalError, WalReader, WalWriter};

/// A bypass-db table.
#[derive(Debug)]
pub struct Table {
    path: PathBuf,
    schema: Schema,
    active: RowBatch,
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
        let wal = WalWriter::open(&wal_path)?;
        Ok(Self {
            path,
            active: RowBatch::empty(&schema),
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
        let timestamp_column = self.schema.timestamp_column().name();
        let timestamps = self
            .active
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
        Ok(ScanResult::from_batch(&self.active.take_indices(&indices)))
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
        let batch = scan_result_to_batch(&time_result);
        let indices = filter_indices(&batch, predicate)?;
        Ok(ScanResult::from_batch(&batch.take_indices(&indices)))
    }

    /// Flush the WAL to durable storage.
    ///
    /// # Errors
    ///
    /// Returns a WAL error when syncing fails.
    pub fn flush(&self) -> Result<(), TableError> {
        self.wal.sync()?;
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

fn scan_result_to_batch(result: &ScanResult) -> RowBatch {
    let columns = result
        .columns()
        .iter()
        .map(|(name, column)| {
            let data = match column {
                crate::scan::ScanColumn::F64(values) => ColumnData::F64(values.to_vec()),
                crate::scan::ScanColumn::I64(values) => ColumnData::I64(values.to_vec()),
                crate::scan::ScanColumn::Timestamp(values) => {
                    ColumnData::Timestamp(values.to_vec())
                }
                crate::scan::ScanColumn::FixedStr { width, values } => ColumnData::FixedStr {
                    width: *width,
                    values: values.to_vec(),
                },
            };
            (name.clone(), data)
        })
        .collect::<Vec<_>>();
    RowBatch::from_parts(columns, result.row_count())
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
            result.column("price").unwrap().as_f64().unwrap(),
            &[2.0, 3.0]
        );
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
        assert_eq!(result.column("volume").unwrap().as_i64().unwrap(), &[300]);

        fs::remove_dir_all(path).unwrap();
    }
}
