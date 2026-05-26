use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::batch::{ColumnData, RowBatch};
use crate::schema::{DType, Schema};
use crate::table::TableError;

/// Name of the manifest file at the table root.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Table manifest stored at `manifest.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Manifest {
    /// Current manifest format version.
    pub version: u32,
    /// Active WAL segment path relative to the table root.
    pub active_wal: String,
    /// Number of WAL records already represented by sealed segments.
    pub wal_records_applied: usize,
    /// Next segment id to allocate.
    pub next_segment_id: u64,
    /// Ordered sealed segment entries.
    pub sealed_segments: Vec<SegmentRef>,
}

impl Manifest {
    /// Create a fresh manifest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: 1,
            active_wal: "WAL/wal-000000.log".to_string(),
            wal_records_applied: 0,
            next_segment_id: 1,
            sealed_segments: Vec::new(),
        }
    }

    /// Load a manifest from disk or create a default one if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or decoded.
    pub fn load(table_path: &Path) -> Result<Self, TableError> {
        let path = table_path.join(MANIFEST_FILE);
        if !path.exists() {
            return Ok(Self::new());
        }
        let text = fs::read_to_string(&path).map_err(|err| TableError::Io(err.to_string()))?;
        serde_json::from_str(&text).map_err(|err| TableError::Manifest(err.to_string()))
    }

    /// Store the manifest to disk.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or file writes fail.
    pub fn store(&self, table_path: &Path) -> Result<(), TableError> {
        let path = table_path.join(MANIFEST_FILE);
        let tmp = table_path.join("manifest.json.tmp");
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| TableError::Manifest(err.to_string()))?;
        fs::write(&tmp, text).map_err(|err| TableError::Io(err.to_string()))?;
        fs::rename(tmp, path).map_err(|err| TableError::Io(err.to_string()))
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

/// Manifest entry for one sealed segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SegmentRef {
    /// Segment id.
    pub id: u64,
    /// Segment directory relative to the table root.
    pub path: String,
    /// Number of rows in the segment.
    pub row_count: usize,
    /// Minimum timestamp, if the segment is non-empty.
    pub min_ts: Option<i64>,
    /// Maximum timestamp, if the segment is non-empty.
    pub max_ts: Option<i64>,
}

/// Metadata stored in each segment's `meta.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SegmentMeta {
    /// Segment id.
    pub id: u64,
    /// Number of rows in the segment.
    pub row_count: usize,
    /// Minimum timestamp, if the segment is non-empty.
    pub min_ts: Option<i64>,
    /// Maximum timestamp, if the segment is non-empty.
    pub max_ts: Option<i64>,
    /// Ordered column metadata.
    pub columns: Vec<ColumnMeta>,
}

impl SegmentMeta {
    fn to_ref(&self) -> SegmentRef {
        SegmentRef {
            id: self.id,
            path: format!("segments/{}", segment_dir_name(self.id)),
            row_count: self.row_count,
            min_ts: self.min_ts,
            max_ts: self.max_ts,
        }
    }
}

/// Metadata for one column file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ColumnMeta {
    /// Column name.
    pub name: String,
    /// Logical type name.
    pub dtype: String,
    /// Fixed string width for `FixedStr`.
    pub fixed_width: Option<usize>,
    /// Column file name relative to the segment directory.
    pub file: String,
}

/// Immutable sealed segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImmutableSegment {
    path: PathBuf,
    meta: SegmentMeta,
}

impl ImmutableSegment {
    /// Load a segment from a manifest entry.
    ///
    /// # Errors
    ///
    /// Returns an error when segment metadata cannot be read.
    pub fn load(table_path: &Path, segment_ref: &SegmentRef) -> Result<Self, TableError> {
        let path = table_path.join(&segment_ref.path);
        let meta = read_meta(&path)?;
        Ok(Self { path, meta })
    }

    /// Segment metadata.
    #[must_use]
    pub fn meta(&self) -> &SegmentMeta {
        &self.meta
    }

    /// Return true if this segment can contain timestamps in `[start, end)`.
    #[must_use]
    pub fn overlaps_time_range(&self, start: i64, end: i64) -> bool {
        match (self.meta.min_ts, self.meta.max_ts) {
            (Some(min_ts), Some(max_ts)) => max_ts >= start && min_ts < end,
            _ => false,
        }
    }

    /// Read the whole segment into a row batch.
    ///
    /// # Errors
    ///
    /// Returns an error when a column file cannot be decoded or does not match
    /// the schema.
    pub fn read_batch(&self, schema: &Schema) -> Result<RowBatch, TableError> {
        let mut columns = Vec::with_capacity(schema.columns().len());
        for column in schema.columns() {
            let data = read_column(
                &self.path,
                column.name(),
                column.dtype(),
                self.meta.row_count,
            )?;
            columns.push((column.name().to_string(), data));
        }
        Ok(RowBatch::from_parts(columns, self.meta.row_count))
    }
}

/// Seal a row batch as an immutable segment.
///
/// # Errors
///
/// Returns an error when metadata or column files cannot be written.
pub fn seal_batch(
    table_path: &Path,
    schema: &Schema,
    segment_id: u64,
    batch: &RowBatch,
) -> Result<ImmutableSegment, TableError> {
    let dir = table_path
        .join("segments")
        .join(segment_dir_name(segment_id));
    fs::create_dir_all(&dir).map_err(|err| TableError::Io(err.to_string()))?;
    let timestamp_column = schema.timestamp_column().name();
    let timestamps = batch
        .column(timestamp_column)
        .and_then(ColumnData::timestamp_values)
        .ok_or_else(|| TableError::MissingColumn {
            column: timestamp_column.to_string(),
        })?;
    let min_ts = timestamps.iter().min().copied();
    let max_ts = timestamps.iter().max().copied();
    let mut columns = Vec::with_capacity(schema.columns().len());

    for column in schema.columns() {
        let file = column_file_name(column.name())?;
        let data = batch
            .column(column.name())
            .ok_or_else(|| TableError::MissingColumn {
                column: column.name().to_string(),
            })?;
        write_column(&dir.join(&file), data)?;
        let (dtype, fixed_width) = dtype_meta(column.dtype());
        columns.push(ColumnMeta {
            name: column.name().to_string(),
            dtype,
            fixed_width,
            file,
        });
    }

    let meta = SegmentMeta {
        id: segment_id,
        row_count: batch.row_count(),
        min_ts,
        max_ts,
        columns,
    };
    write_meta(&dir, &meta)?;
    Ok(ImmutableSegment { path: dir, meta })
}

pub(crate) fn segment_ref(segment: &ImmutableSegment) -> SegmentRef {
    segment.meta.to_ref()
}

fn read_meta(dir: &Path) -> Result<SegmentMeta, TableError> {
    let text =
        fs::read_to_string(dir.join("meta.json")).map_err(|err| TableError::Io(err.to_string()))?;
    serde_json::from_str(&text).map_err(|err| TableError::Segment(err.to_string()))
}

fn write_meta(dir: &Path, meta: &SegmentMeta) -> Result<(), TableError> {
    let text =
        serde_json::to_string_pretty(meta).map_err(|err| TableError::Segment(err.to_string()))?;
    fs::write(dir.join("meta.json"), text).map_err(|err| TableError::Io(err.to_string()))
}

fn write_column(path: &Path, data: &ColumnData) -> Result<(), TableError> {
    let mut out = Vec::new();
    match data {
        ColumnData::F64(values) => {
            out.reserve(values.len() * 8);
            for value in values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        ColumnData::I64(values) | ColumnData::Timestamp(values) => {
            out.reserve(values.len() * 8);
            for value in values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        ColumnData::FixedStr { width, values } => {
            out.reserve(values.len() * width);
            for value in values {
                if value.len() != *width {
                    return Err(TableError::Segment(format!(
                        "fixed string value in {} is not {width} bytes",
                        path.display()
                    )));
                }
                out.extend_from_slice(value);
            }
        }
    }
    fs::write(path, out).map_err(|err| TableError::Io(err.to_string()))
}

fn read_column(
    dir: &Path,
    name: &str,
    dtype: &DType,
    row_count: usize,
) -> Result<ColumnData, TableError> {
    let file = column_file_name(name)?;
    let bytes = fs::read(dir.join(file)).map_err(|err| TableError::Io(err.to_string()))?;
    match dtype {
        DType::F64 => {
            let values = read_f64_values(&bytes, row_count)?;
            Ok(ColumnData::F64(values))
        }
        DType::I64 => {
            let values = read_i64_values(&bytes, row_count)?;
            Ok(ColumnData::I64(values))
        }
        DType::Timestamp => {
            let values = read_i64_values(&bytes, row_count)?;
            Ok(ColumnData::Timestamp(values))
        }
        DType::FixedStr(width) => {
            let expected = row_count
                .checked_mul(*width)
                .ok_or_else(|| TableError::Segment("fixed string byte count overflow".into()))?;
            if bytes.len() != expected {
                return Err(TableError::Segment(format!(
                    "fixed string column {name} has {} bytes, expected {expected}",
                    bytes.len()
                )));
            }
            let values = bytes
                .chunks_exact(*width)
                .map(|chunk| chunk.to_vec())
                .collect();
            Ok(ColumnData::FixedStr {
                width: *width,
                values,
            })
        }
    }
}

fn read_f64_values(bytes: &[u8], row_count: usize) -> Result<Vec<f64>, TableError> {
    if bytes.len() != row_count * 8 {
        return Err(TableError::Segment(format!(
            "f64 column has {} bytes, expected {}",
            bytes.len(),
            row_count * 8
        )));
    }
    Ok(bytes
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes")))
        .collect())
}

fn read_i64_values(bytes: &[u8], row_count: usize) -> Result<Vec<i64>, TableError> {
    if bytes.len() != row_count * 8 {
        return Err(TableError::Segment(format!(
            "i64 column has {} bytes, expected {}",
            bytes.len(),
            row_count * 8
        )));
    }
    Ok(bytes
        .chunks_exact(8)
        .map(|chunk| i64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes")))
        .collect())
}

fn dtype_meta(dtype: &DType) -> (String, Option<usize>) {
    match dtype {
        DType::F64 => ("F64".to_string(), None),
        DType::I64 => ("I64".to_string(), None),
        DType::Timestamp => ("Timestamp".to_string(), None),
        DType::FixedStr(width) => ("FixedStr".to_string(), Some(*width)),
    }
}

fn column_file_name(name: &str) -> Result<String, TableError> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(TableError::Segment(format!(
            "column name {name:?} is not valid for a segment file"
        )));
    }
    Ok(format!("{name}.col"))
}

fn segment_dir_name(segment_id: u64) -> String {
    format!("seg-{segment_id:06}")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::batch::{ColumnData, RowBatch};
    use crate::schema::{ColumnDef, DType, Schema};

    use super::{seal_batch, ImmutableSegment, Manifest};

    static NEXT_SEGMENT: AtomicUsize = AtomicUsize::new(0);

    fn table_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "bypass-db-segment-{name}-{}-{}",
            process::id(),
            NEXT_SEGMENT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn schema() -> Schema {
        Schema::new(
            "ticks",
            vec![
                ColumnDef::new("timestamp", DType::Timestamp).unwrap(),
                ColumnDef::new("price", DType::F64).unwrap(),
                ColumnDef::new("symbol", DType::FixedStr(4)).unwrap(),
            ],
        )
        .unwrap()
    }

    fn batch(schema: &Schema) -> RowBatch {
        RowBatch::builder(schema)
            .column("timestamp", ColumnData::Timestamp(vec![10, 20]))
            .column("price", ColumnData::F64(vec![1.5, 2.5]))
            .column(
                "symbol",
                ColumnData::FixedStr {
                    width: 4,
                    values: vec![b"MSFT".to_vec(), b"AAPL".to_vec()],
                },
            )
            .build()
            .unwrap()
    }

    #[test]
    fn sealed_segment_round_trips_batch_and_manifest() {
        let path = table_path("round-trip");
        fs::create_dir_all(path.join("segments")).unwrap();
        let schema = schema();
        let batch = batch(&schema);
        let segment = seal_batch(&path, &schema, 1, &batch).unwrap();
        assert_eq!(segment.meta().row_count, 2);
        assert!(segment.overlaps_time_range(15, 25));
        assert_eq!(segment.read_batch(&schema).unwrap(), batch);

        let mut manifest = Manifest::new();
        manifest.sealed_segments.push(super::segment_ref(&segment));
        manifest.next_segment_id = 2;
        manifest.store(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        let loaded_segment = ImmutableSegment::load(&path, &loaded.sealed_segments[0]).unwrap();
        assert_eq!(loaded_segment.read_batch(&schema).unwrap(), batch);

        fs::remove_dir_all(path).unwrap();
    }
}
