use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::batch::{ColumnData, RowBatch};
use crate::schema::{DType, Schema};
use crate::segment::MappedColumn;

/// Result of a table scan.
///
/// A scan result is chunked. Sealed segment chunks can remain backed by
/// read-only memory maps, while active in-memory rows are represented as owned
/// chunks. Use the `*_values` helpers when callers need a contiguous `Vec`.
#[derive(Clone, Debug)]
pub struct ScanResult {
    columns: Vec<(String, ScanColumn)>,
    row_count: usize,
}

impl ScanResult {
    /// Build a scan result from a row batch.
    #[must_use]
    pub fn from_batch(batch: &RowBatch) -> Self {
        let columns = batch
            .columns()
            .iter()
            .map(|(name, data)| (name.clone(), ScanColumn::from(data.clone())))
            .collect();
        Self {
            columns,
            row_count: batch.row_count(),
        }
    }

    pub(crate) fn empty(schema: &Schema) -> Self {
        let columns = schema
            .columns()
            .iter()
            .map(|column| (column.name().to_string(), ScanColumn::empty(column.dtype())))
            .collect();
        Self {
            columns,
            row_count: 0,
        }
    }

    pub(crate) fn append_batch(&mut self, batch: &RowBatch) -> Result<(), ScanError> {
        self.append_columns(
            batch
                .columns()
                .iter()
                .map(|(name, data)| (name.as_str(), ScanColumn::from(data.clone()))),
            batch.row_count(),
        )
    }

    pub(crate) fn append_mapped_columns(
        &mut self,
        columns: Vec<(String, Arc<MappedColumn>)>,
        selection: RowSelection,
    ) -> Result<(), ScanError> {
        let row_count = selection.len();
        self.append_columns(
            columns
                .into_iter()
                .map(|(name, column)| (name, ScanColumn::from_mapped(column, selection.clone()))),
            row_count,
        )
    }

    fn append_columns<I, N>(&mut self, columns: I, row_count: usize) -> Result<(), ScanError>
    where
        I: IntoIterator<Item = (N, ScanColumn)>,
        N: AsRef<str>,
    {
        let mut count = 0usize;
        for (incoming, (expected_name, existing)) in
            columns.into_iter().zip(self.columns.iter_mut())
        {
            if incoming.0.as_ref() != expected_name {
                return Err(ScanError::ColumnOrderMismatch);
            }
            existing.append(incoming.1)?;
            count = count.saturating_add(1);
        }
        if count != self.columns.len() {
            return Err(ScanError::ColumnOrderMismatch);
        }
        self.row_count = self
            .row_count
            .checked_add(row_count)
            .ok_or(ScanError::RowCountOverflow)?;
        Ok(())
    }

    pub(crate) fn take_indices(&self, indices: &[usize]) -> Self {
        let columns = self
            .columns
            .iter()
            .map(|(name, column)| (name.clone(), column.take_indices(indices)))
            .collect();
        Self {
            columns,
            row_count: indices.len(),
        }
    }

    /// Number of rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Ordered scan columns.
    #[must_use]
    pub fn columns(&self) -> &[(String, ScanColumn)] {
        &self.columns
    }

    /// Return a column by name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ScanColumn> {
        self.columns
            .iter()
            .find_map(|(column, data)| (column == name).then_some(data))
    }
}

impl PartialEq for ScanResult {
    fn eq(&self, other: &Self) -> bool {
        self.row_count == other.row_count && self.columns == other.columns
    }
}

/// Column data returned by a scan.
#[derive(Clone, Debug)]
pub enum ScanColumn {
    /// `f64` values.
    F64(Arc<[F64Chunk]>),
    /// `i64` values.
    I64(Arc<[I64Chunk]>),
    /// Timestamp values in nanoseconds since Unix epoch.
    Timestamp(Arc<[I64Chunk]>),
    /// Fixed-width byte strings.
    FixedStr {
        width: usize,
        chunks: Arc<[FixedStrChunk]>,
    },
}

/// One `f64` scan chunk.
#[derive(Clone, Debug)]
pub enum F64Chunk {
    /// Owned values, usually from the active mutable segment.
    Owned(Arc<[f64]>),
    /// Values selected from a mapped sealed segment column.
    Mapped(MappedSelection),
}

/// One `i64` or timestamp scan chunk.
#[derive(Clone, Debug)]
pub enum I64Chunk {
    /// Owned values, usually from the active mutable segment.
    Owned(Arc<[i64]>),
    /// Values selected from a mapped sealed segment column.
    Mapped(MappedSelection),
}

/// One fixed-width byte-string scan chunk.
#[derive(Clone, Debug)]
pub enum FixedStrChunk {
    /// Owned values, usually from the active mutable segment.
    Owned(Arc<[Vec<u8>]>),
    /// Values selected from a mapped sealed segment column.
    Mapped(MappedSelection),
}

/// Row selection over one mapped column.
#[derive(Clone, Debug)]
pub struct MappedSelection {
    column: Arc<MappedColumn>,
    rows: RowSelection,
}

#[derive(Clone, Debug)]
pub(crate) enum RowSelection {
    All { len: usize },
    Range { start: usize, len: usize },
    Indices(Arc<[usize]>),
}

impl RowSelection {
    pub(crate) fn from_indices(indices: Vec<usize>, total_rows: usize) -> Self {
        if indices.len() == total_rows && indices.iter().copied().eq(0..total_rows) {
            return Self::All { len: total_rows };
        }
        let Some((&first, rest)) = indices.split_first() else {
            return Self::Range { start: 0, len: 0 };
        };
        if rest
            .iter()
            .copied()
            .enumerate()
            .all(|(offset, value)| value == first + offset + 1)
        {
            return Self::Range {
                start: first,
                len: indices.len(),
            };
        }
        Self::Indices(indices.into())
    }

    fn len(&self) -> usize {
        match self {
            Self::All { len } | Self::Range { len, .. } => *len,
            Self::Indices(indices) => indices.len(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_identity(&self) -> bool {
        matches!(self, Self::All { .. })
    }

    fn index_at(&self, local_idx: usize) -> Option<usize> {
        match self {
            Self::All { len } => (local_idx < *len).then_some(local_idx),
            Self::Range { start, len } => (local_idx < *len).then_some(start + local_idx),
            Self::Indices(indices) => indices.get(local_idx).copied(),
        }
    }

    fn iter(&self) -> RowSelectionIter<'_> {
        RowSelectionIter {
            selection: self,
            next: 0,
        }
    }

    fn take_local_indices(&self, local_indices: &[usize]) -> Self {
        Self::from_indices(
            local_indices
                .iter()
                .filter_map(|&local_idx| self.index_at(local_idx))
                .collect(),
            self.len(),
        )
    }
}

struct RowSelectionIter<'a> {
    selection: &'a RowSelection,
    next: usize,
}

impl Iterator for RowSelectionIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.selection.index_at(self.next)?;
        self.next += 1;
        Some(value)
    }
}

impl ScanColumn {
    fn empty(dtype: &DType) -> Self {
        match dtype {
            DType::F64 => Self::F64(Arc::from([])),
            DType::I64 => Self::I64(Arc::from([])),
            DType::Timestamp => Self::Timestamp(Arc::from([])),
            DType::FixedStr(width) => Self::FixedStr {
                width: *width,
                chunks: Arc::from([]),
            },
        }
    }

    fn from_mapped(column: Arc<MappedColumn>, rows: RowSelection) -> Self {
        let selection = MappedSelection { column, rows };
        match selection.column.dtype() {
            DType::F64 => Self::F64(Arc::from([F64Chunk::Mapped(selection)])),
            DType::I64 => Self::I64(Arc::from([I64Chunk::Mapped(selection)])),
            DType::Timestamp => Self::Timestamp(Arc::from([I64Chunk::Mapped(selection)])),
            DType::FixedStr(width) => Self::FixedStr {
                width: *width,
                chunks: Arc::from([FixedStrChunk::Mapped(selection)]),
            },
        }
    }

    fn append(&mut self, other: Self) -> Result<(), ScanError> {
        match (self, other) {
            (Self::F64(left), Self::F64(right)) => append_arc_chunks(left, &right),
            (Self::I64(left), Self::I64(right))
            | (Self::Timestamp(left), Self::Timestamp(right)) => append_arc_chunks(left, &right),
            (
                Self::FixedStr {
                    width: left_width,
                    chunks: left,
                },
                Self::FixedStr {
                    width: right_width,
                    chunks: right,
                },
            ) if *left_width == right_width => append_arc_chunks(left, &right),
            _ => Err(ScanError::ColumnTypeMismatch),
        }
    }

    fn take_indices(&self, indices: &[usize]) -> Self {
        match self {
            Self::F64(chunks) => Self::F64(take_f64_chunks(chunks, indices)),
            Self::I64(chunks) => Self::I64(take_i64_chunks(chunks, indices)),
            Self::Timestamp(chunks) => Self::Timestamp(take_i64_chunks(chunks, indices)),
            Self::FixedStr { width, chunks } => Self::FixedStr {
                width: *width,
                chunks: take_fixed_str_chunks(chunks, indices),
            },
        }
    }

    /// Borrow as `f64` values when this column is a single owned chunk.
    ///
    /// Mmap-backed or multi-chunk columns should use [`ScanColumn::f64_values`]
    /// to materialize a contiguous vector.
    #[must_use]
    pub fn as_f64(&self) -> Option<&[f64]> {
        match self {
            Self::F64(chunks) if chunks.len() == 1 => match &chunks[0] {
                F64Chunk::Owned(values) => Some(values),
                F64Chunk::Mapped(_) => None,
            },
            _ => None,
        }
    }

    /// Borrow as `i64` values when this column is a single owned chunk.
    ///
    /// Mmap-backed or multi-chunk columns should use [`ScanColumn::i64_values`]
    /// to materialize a contiguous vector.
    #[must_use]
    pub fn as_i64(&self) -> Option<&[i64]> {
        match self {
            Self::I64(chunks) | Self::Timestamp(chunks) if chunks.len() == 1 => match &chunks[0] {
                I64Chunk::Owned(values) => Some(values),
                I64Chunk::Mapped(_) => None,
            },
            _ => None,
        }
    }

    /// Borrow as timestamp values when this column is a single owned chunk.
    ///
    /// Mmap-backed or multi-chunk columns should use
    /// [`ScanColumn::timestamp_values`] to materialize a contiguous vector.
    #[must_use]
    pub fn as_timestamps(&self) -> Option<&[i64]> {
        match self {
            Self::Timestamp(chunks) if chunks.len() == 1 => match &chunks[0] {
                I64Chunk::Owned(values) => Some(values),
                I64Chunk::Mapped(_) => None,
            },
            _ => None,
        }
    }

    /// Return all `f64` values as a contiguous vector.
    #[must_use]
    pub fn f64_values(&self) -> Option<Vec<f64>> {
        let Self::F64(chunks) = self else {
            return None;
        };
        Some(flatten_f64_chunks(chunks))
    }

    /// Return all `i64` or timestamp values as a contiguous vector.
    #[must_use]
    pub fn i64_values(&self) -> Option<Vec<i64>> {
        match self {
            Self::I64(chunks) | Self::Timestamp(chunks) => Some(flatten_i64_chunks(chunks)),
            _ => None,
        }
    }

    /// Return all timestamp values as a contiguous vector.
    #[must_use]
    pub fn timestamp_values(&self) -> Option<Vec<i64>> {
        let Self::Timestamp(chunks) = self else {
            return None;
        };
        Some(flatten_i64_chunks(chunks))
    }

    /// Return all fixed-width byte-string values as a contiguous vector.
    #[must_use]
    pub fn fixed_str_values(&self) -> Option<Vec<Vec<u8>>> {
        let Self::FixedStr { chunks, .. } = self else {
            return None;
        };
        Some(flatten_fixed_str_chunks(chunks))
    }

    /// Return true when at least one chunk is mmap-backed.
    #[must_use]
    pub fn has_mapped_chunks(&self) -> bool {
        match self {
            Self::F64(chunks) => chunks
                .iter()
                .any(|chunk| matches!(chunk, F64Chunk::Mapped(_))),
            Self::I64(chunks) | Self::Timestamp(chunks) => chunks
                .iter()
                .any(|chunk| matches!(chunk, I64Chunk::Mapped(_))),
            Self::FixedStr { chunks, .. } => chunks
                .iter()
                .any(|chunk| matches!(chunk, FixedStrChunk::Mapped(_))),
        }
    }

    fn test_at(&self, index: usize, predicate: &dyn ColumnPredicate) -> bool {
        match self {
            Self::F64(chunks) => {
                value_at_f64(chunks, index).is_some_and(|value| predicate.test_f64(value))
            }
            Self::I64(chunks) | Self::Timestamp(chunks) => {
                value_at_i64(chunks, index).is_some_and(|value| predicate.test_i64(value))
            }
            Self::FixedStr { chunks, .. } => value_at_fixed_str(chunks, index)
                .is_some_and(|value| predicate.test_fixed_str(&value)),
        }
    }

    fn range_filter_f64(&self, min: f64, max: f64) -> Option<Vec<usize>> {
        let Self::F64(chunks) = self else {
            return None;
        };
        let mut output = Vec::new();
        let mut base = 0usize;
        for chunk in chunks.iter() {
            let local = match chunk {
                F64Chunk::Owned(values) => simd::range_filter_f64_values(values, min, max),
                F64Chunk::Mapped(selection) => {
                    simd::range_filter_f64_selection(selection, min, max)
                }
            };
            output.extend(local.into_iter().map(|idx| base + idx));
            base += chunk.row_count();
        }
        Some(output)
    }

    fn gt_filter_f64(&self, threshold: f64) -> Option<Vec<usize>> {
        let Self::F64(chunks) = self else {
            return None;
        };
        let mut output = Vec::new();
        let mut base = 0usize;
        for chunk in chunks.iter() {
            let local = match chunk {
                F64Chunk::Owned(values) => simd::gt_filter_f64_values(values, threshold),
                F64Chunk::Mapped(selection) => simd::gt_filter_f64_selection(selection, threshold),
            };
            output.extend(local.into_iter().map(|idx| base + idx));
            base += chunk.row_count();
        }
        Some(output)
    }
}

impl PartialEq for ScanColumn {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::F64(_), Self::F64(_)) => self.f64_values() == other.f64_values(),
            (Self::I64(_), Self::I64(_)) | (Self::Timestamp(_), Self::Timestamp(_)) => {
                self.i64_values() == other.i64_values()
            }
            (
                Self::FixedStr {
                    width: left_width, ..
                },
                Self::FixedStr {
                    width: right_width, ..
                },
            ) => left_width == right_width && self.fixed_str_values() == other.fixed_str_values(),
            _ => false,
        }
    }
}

impl F64Chunk {
    fn row_count(&self) -> usize {
        match self {
            Self::Owned(values) => values.len(),
            Self::Mapped(selection) => selection.rows.len(),
        }
    }
}

impl I64Chunk {
    fn row_count(&self) -> usize {
        match self {
            Self::Owned(values) => values.len(),
            Self::Mapped(selection) => selection.rows.len(),
        }
    }
}

impl FixedStrChunk {
    fn row_count(&self) -> usize {
        match self {
            Self::Owned(values) => values.len(),
            Self::Mapped(selection) => selection.rows.len(),
        }
    }
}

impl From<ColumnData> for ScanColumn {
    fn from(value: ColumnData) -> Self {
        match value {
            ColumnData::F64(values) => Self::F64(Arc::from([F64Chunk::Owned(values.into())])),
            ColumnData::I64(values) => Self::I64(Arc::from([I64Chunk::Owned(values.into())])),
            ColumnData::Timestamp(values) => {
                Self::Timestamp(Arc::from([I64Chunk::Owned(values.into())]))
            }
            ColumnData::FixedStr { width, values } => Self::FixedStr {
                width,
                chunks: Arc::from([FixedStrChunk::Owned(values.into())]),
            },
        }
    }
}

/// Predicate over one table column.
pub trait ColumnPredicate: Send + Sync {
    /// Column targeted by this predicate.
    fn column(&self) -> &str;

    /// Return an `f64` range when this predicate is exactly `min <= x < max`.
    fn f64_range(&self) -> Option<(f64, f64)> {
        None
    }

    /// Return an `f64` threshold when this predicate is exactly `x > threshold`.
    fn f64_gt(&self) -> Option<f64> {
        None
    }

    /// Test an `f64` value.
    fn test_f64(&self, _value: f64) -> bool {
        false
    }

    /// Test an `i64` value.
    fn test_i64(&self, _value: i64) -> bool {
        false
    }

    /// Test a fixed-width byte string.
    fn test_fixed_str(&self, _value: &[u8]) -> bool {
        false
    }
}

/// `min <= value < max` predicate for numeric columns.
#[derive(Clone, Debug, PartialEq)]
pub struct RangePredicate {
    column: String,
    min: f64,
    max: f64,
}

impl RangePredicate {
    /// Create a range predicate.
    #[must_use]
    pub fn new(column: impl Into<String>, min: f64, max: f64) -> Self {
        Self {
            column: column.into(),
            min,
            max,
        }
    }
}

impl ColumnPredicate for RangePredicate {
    fn column(&self) -> &str {
        &self.column
    }

    fn f64_range(&self) -> Option<(f64, f64)> {
        Some((self.min, self.max))
    }

    fn test_f64(&self, value: f64) -> bool {
        value >= self.min && value < self.max
    }

    fn test_i64(&self, value: i64) -> bool {
        let value = value as f64;
        value >= self.min && value < self.max
    }
}

/// `value > threshold` predicate for numeric columns.
#[derive(Clone, Debug, PartialEq)]
pub struct GtPredicate {
    column: String,
    threshold: f64,
}

impl GtPredicate {
    /// Create a greater-than predicate.
    #[must_use]
    pub fn new(column: impl Into<String>, threshold: f64) -> Self {
        Self {
            column: column.into(),
            threshold,
        }
    }
}

impl ColumnPredicate for GtPredicate {
    fn column(&self) -> &str {
        &self.column
    }

    fn test_f64(&self, value: f64) -> bool {
        value > self.threshold
    }

    fn f64_gt(&self) -> Option<f64> {
        Some(self.threshold)
    }

    fn test_i64(&self, value: i64) -> bool {
        (value as f64) > self.threshold
    }
}

/// Exact scalar predicate for fixed-width string columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScalarPredicate {
    column: String,
    value: Vec<u8>,
}

impl ScalarPredicate {
    /// Create an exact byte-string predicate.
    #[must_use]
    pub fn new(column: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            column: column.into(),
            value: value.into(),
        }
    }
}

impl ColumnPredicate for ScalarPredicate {
    fn column(&self) -> &str {
        &self.column
    }

    fn test_fixed_str(&self, value: &[u8]) -> bool {
        value == self.value
    }
}

#[cfg(test)]
pub(crate) fn filter_indices(
    batch: &RowBatch,
    predicate: &dyn ColumnPredicate,
) -> Result<Vec<usize>, ScanError> {
    let data = batch
        .column(predicate.column())
        .ok_or_else(|| ScanError::UnknownColumn {
            column: predicate.column().to_string(),
        })?;

    let mut indices = Vec::new();
    match data {
        ColumnData::F64(values) => {
            if let Some((min, max)) = predicate.f64_range() {
                return Ok(simd::range_filter_f64_values(values, min, max));
            }
            indices.extend(
                values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &value)| predicate.test_f64(value).then_some(idx)),
            );
        }
        ColumnData::I64(values) | ColumnData::Timestamp(values) => {
            indices.extend(
                values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &value)| predicate.test_i64(value).then_some(idx)),
            );
        }
        ColumnData::FixedStr { values, .. } => {
            indices.extend(
                values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, value)| predicate.test_fixed_str(value).then_some(idx)),
            );
        }
    }
    Ok(indices)
}

pub(crate) fn filter_scan_indices(
    result: &ScanResult,
    predicate: &dyn ColumnPredicate,
) -> Result<Vec<usize>, ScanError> {
    let column = result
        .column(predicate.column())
        .ok_or_else(|| ScanError::UnknownColumn {
            column: predicate.column().to_string(),
        })?;
    if let Some((min, max)) = predicate.f64_range() {
        if let Some(indices) = column.range_filter_f64(min, max) {
            return Ok(indices);
        }
    }
    if let Some(threshold) = predicate.f64_gt() {
        if let Some(indices) = column.gt_filter_f64(threshold) {
            return Ok(indices);
        }
    }
    Ok((0..result.row_count())
        .filter(|&idx| column.test_at(idx, predicate))
        .collect())
}

pub(crate) fn range_filter_i64_mapped(
    column: &MappedColumn,
    start: i64,
    end: i64,
) -> Result<Vec<usize>, ScanError> {
    simd::range_filter_i64_bytes(column.as_bytes(), start, end)
}

/// Scan error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanError {
    /// Predicate referenced an unknown column.
    UnknownColumn {
        /// Unknown column name.
        column: String,
    },
    /// Columns were appended in a different order than the schema.
    ColumnOrderMismatch,
    /// A scan column append mixed incompatible logical types.
    ColumnTypeMismatch,
    /// Scan result row count overflowed.
    RowCountOverflow,
    /// A byte-backed numeric column had an invalid length.
    InvalidColumnBytes {
        /// Actual byte length.
        len: usize,
        /// Required alignment in bytes.
        width: usize,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownColumn { column } => write!(f, "unknown scan column {column}"),
            Self::ColumnOrderMismatch => write!(f, "scan column order does not match schema"),
            Self::ColumnTypeMismatch => write!(f, "scan column type mismatch"),
            Self::RowCountOverflow => write!(f, "scan result row count overflow"),
            Self::InvalidColumnBytes { len, width } => {
                write!(f, "column has {len} bytes, expected a multiple of {width}")
            }
        }
    }
}

impl Error for ScanError {}

fn append_arc_chunks<T: Clone>(left: &mut Arc<[T]>, right: &[T]) -> Result<(), ScanError> {
    let mut merged = Vec::with_capacity(left.len().saturating_add(right.len()));
    merged.extend(left.iter().cloned());
    merged.extend(right.iter().cloned());
    *left = merged.into();
    Ok(())
}

fn flatten_f64_chunks(chunks: &[F64Chunk]) -> Vec<f64> {
    let mut output = Vec::with_capacity(chunks.iter().map(F64Chunk::row_count).sum());
    for chunk in chunks {
        match chunk {
            F64Chunk::Owned(values) => output.extend(values.iter().copied()),
            F64Chunk::Mapped(selection) => output.extend(
                selection
                    .rows
                    .iter()
                    .filter_map(|idx| selection.column.f64_value(idx)),
            ),
        }
    }
    output
}

fn flatten_i64_chunks(chunks: &[I64Chunk]) -> Vec<i64> {
    let mut output = Vec::with_capacity(chunks.iter().map(I64Chunk::row_count).sum());
    for chunk in chunks {
        match chunk {
            I64Chunk::Owned(values) => output.extend(values.iter().copied()),
            I64Chunk::Mapped(selection) => output.extend(
                selection
                    .rows
                    .iter()
                    .filter_map(|idx| selection.column.i64_value(idx)),
            ),
        }
    }
    output
}

fn flatten_fixed_str_chunks(chunks: &[FixedStrChunk]) -> Vec<Vec<u8>> {
    let mut output = Vec::with_capacity(chunks.iter().map(FixedStrChunk::row_count).sum());
    for chunk in chunks {
        match chunk {
            FixedStrChunk::Owned(values) => output.extend(values.iter().cloned()),
            FixedStrChunk::Mapped(selection) => output.extend(
                selection
                    .rows
                    .iter()
                    .filter_map(|idx| selection.column.fixed_str_value(idx).map(<[u8]>::to_vec)),
            ),
        }
    }
    output
}

fn take_f64_chunks(chunks: &[F64Chunk], indices: &[usize]) -> Arc<[F64Chunk]> {
    let mut output = Vec::new();
    for (chunk_idx, local_indices) in group_indices_by_chunk(
        indices,
        chunks
            .iter()
            .map(F64Chunk::row_count)
            .collect::<Vec<_>>()
            .as_slice(),
    ) {
        match &chunks[chunk_idx] {
            F64Chunk::Owned(values) => output.push(F64Chunk::Owned(
                local_indices.iter().map(|&idx| values[idx]).collect(),
            )),
            F64Chunk::Mapped(selection) => output.push(F64Chunk::Mapped(MappedSelection {
                column: Arc::clone(&selection.column),
                rows: selection.rows.take_local_indices(&local_indices),
            })),
        }
    }
    output.into()
}

fn take_i64_chunks(chunks: &[I64Chunk], indices: &[usize]) -> Arc<[I64Chunk]> {
    let mut output = Vec::new();
    for (chunk_idx, local_indices) in group_indices_by_chunk(
        indices,
        chunks
            .iter()
            .map(I64Chunk::row_count)
            .collect::<Vec<_>>()
            .as_slice(),
    ) {
        match &chunks[chunk_idx] {
            I64Chunk::Owned(values) => output.push(I64Chunk::Owned(
                local_indices.iter().map(|&idx| values[idx]).collect(),
            )),
            I64Chunk::Mapped(selection) => output.push(I64Chunk::Mapped(MappedSelection {
                column: Arc::clone(&selection.column),
                rows: selection.rows.take_local_indices(&local_indices),
            })),
        }
    }
    output.into()
}

fn take_fixed_str_chunks(chunks: &[FixedStrChunk], indices: &[usize]) -> Arc<[FixedStrChunk]> {
    let mut output = Vec::new();
    for (chunk_idx, local_indices) in group_indices_by_chunk(
        indices,
        chunks
            .iter()
            .map(FixedStrChunk::row_count)
            .collect::<Vec<_>>()
            .as_slice(),
    ) {
        match &chunks[chunk_idx] {
            FixedStrChunk::Owned(values) => output.push(FixedStrChunk::Owned(
                local_indices
                    .iter()
                    .map(|&idx| values[idx].clone())
                    .collect(),
            )),
            FixedStrChunk::Mapped(selection) => {
                output.push(FixedStrChunk::Mapped(MappedSelection {
                    column: Arc::clone(&selection.column),
                    rows: selection.rows.take_local_indices(&local_indices),
                }))
            }
        }
    }
    output.into()
}

fn group_indices_by_chunk(indices: &[usize], chunk_lengths: &[usize]) -> Vec<(usize, Vec<usize>)> {
    let mut groups = Vec::<(usize, Vec<usize>)>::new();
    for &global_idx in indices {
        let mut base = 0usize;
        for (chunk_idx, &len) in chunk_lengths.iter().enumerate() {
            let end = base.saturating_add(len);
            if global_idx >= base && global_idx < end {
                match groups.last_mut() {
                    Some((last_chunk, values)) if *last_chunk == chunk_idx => {
                        values.push(global_idx - base)
                    }
                    _ => groups.push((chunk_idx, vec![global_idx - base])),
                }
                break;
            }
            base = end;
        }
    }
    groups
}

fn value_at_f64(chunks: &[F64Chunk], mut index: usize) -> Option<f64> {
    for chunk in chunks {
        let row_count = chunk.row_count();
        if index < row_count {
            return match chunk {
                F64Chunk::Owned(values) => values.get(index).copied(),
                F64Chunk::Mapped(selection) => selection
                    .rows
                    .index_at(index)
                    .and_then(|mapped_idx| selection.column.f64_value(mapped_idx)),
            };
        }
        index -= row_count;
    }
    None
}

fn value_at_i64(chunks: &[I64Chunk], mut index: usize) -> Option<i64> {
    for chunk in chunks {
        let row_count = chunk.row_count();
        if index < row_count {
            return match chunk {
                I64Chunk::Owned(values) => values.get(index).copied(),
                I64Chunk::Mapped(selection) => selection
                    .rows
                    .index_at(index)
                    .and_then(|mapped_idx| selection.column.i64_value(mapped_idx)),
            };
        }
        index -= row_count;
    }
    None
}

fn value_at_fixed_str(chunks: &[FixedStrChunk], mut index: usize) -> Option<Vec<u8>> {
    for chunk in chunks {
        let row_count = chunk.row_count();
        if index < row_count {
            return match chunk {
                FixedStrChunk::Owned(values) => values.get(index).cloned(),
                FixedStrChunk::Mapped(selection) => selection
                    .rows
                    .index_at(index)
                    .and_then(|mapped_idx| selection.column.fixed_str_value(mapped_idx))
                    .map(<[u8]>::to_vec),
            };
        }
        index -= row_count;
    }
    None
}

mod simd {
    use super::{MappedSelection, ScanError};

    pub(super) fn range_filter_i64_bytes(
        bytes: &[u8],
        start: i64,
        end: i64,
    ) -> Result<Vec<usize>, ScanError> {
        if !bytes.len().is_multiple_of(8) {
            return Err(ScanError::InvalidColumnBytes {
                len: bytes.len(),
                width: 8,
            });
        }

        #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                // Safety: AVX2 support is checked at runtime and the byte
                // slice length is validated as a multiple of 8.
                return Ok(unsafe { range_filter_i64_bytes_avx2(bytes, start, end) });
            }
        }

        Ok(bytes
            .chunks_exact(8)
            .enumerate()
            .filter_map(|(idx, chunk)| {
                let value = i64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes"));
                (value >= start && value < end).then_some(idx)
            })
            .collect())
    }

    pub(super) fn range_filter_f64_values(values: &[f64], min: f64, max: f64) -> Vec<usize> {
        #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
        {
            if std::arch::is_x86_feature_detected!("avx") {
                // Safety: AVX support is checked at runtime and `values`
                // points to initialized f64 elements.
                return unsafe { range_filter_f64_values_avx(values, min, max) };
            }
        }

        scalar_range_filter_f64(values.iter().copied(), min, max)
    }

    pub(super) fn gt_filter_f64_values(values: &[f64], threshold: f64) -> Vec<usize> {
        #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
        {
            if std::arch::is_x86_feature_detected!("avx") {
                // Safety: AVX support is checked at runtime and `values`
                // points to initialized f64 elements.
                return unsafe { gt_filter_f64_values_avx(values, threshold) };
            }
        }

        scalar_gt_filter_f64(values.iter().copied(), threshold)
    }

    pub(super) fn range_filter_f64_selection(
        selection: &MappedSelection,
        min: f64,
        max: f64,
    ) -> Vec<usize> {
        if selection.rows.is_identity() {
            if let Ok(indices) = range_filter_f64_bytes(selection.column.as_bytes(), min, max) {
                return indices;
            }
        }
        selection
            .rows
            .iter()
            .enumerate()
            .filter_map(|(local_idx, mapped_idx)| {
                selection
                    .column
                    .f64_value(mapped_idx)
                    .filter(|value| *value >= min && *value < max)
                    .map(|_| local_idx)
            })
            .collect()
    }

    pub(super) fn gt_filter_f64_selection(
        selection: &MappedSelection,
        threshold: f64,
    ) -> Vec<usize> {
        if selection.rows.is_identity() {
            if let Ok(indices) = gt_filter_f64_bytes(selection.column.as_bytes(), threshold) {
                return indices;
            }
        }
        selection
            .rows
            .iter()
            .enumerate()
            .filter_map(|(local_idx, mapped_idx)| {
                selection
                    .column
                    .f64_value(mapped_idx)
                    .filter(|value| *value > threshold)
                    .map(|_| local_idx)
            })
            .collect()
    }

    fn range_filter_f64_bytes(bytes: &[u8], min: f64, max: f64) -> Result<Vec<usize>, ScanError> {
        if !bytes.len().is_multiple_of(8) {
            return Err(ScanError::InvalidColumnBytes {
                len: bytes.len(),
                width: 8,
            });
        }

        #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
        {
            if std::arch::is_x86_feature_detected!("avx") {
                // Safety: AVX support is checked at runtime and the byte
                // slice length is validated as a multiple of 8.
                return Ok(unsafe { range_filter_f64_bytes_avx(bytes, min, max) });
            }
        }

        Ok(bytes
            .chunks_exact(8)
            .enumerate()
            .filter_map(|(idx, chunk)| {
                let value = f64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes"));
                (value >= min && value < max).then_some(idx)
            })
            .collect())
    }

    fn gt_filter_f64_bytes(bytes: &[u8], threshold: f64) -> Result<Vec<usize>, ScanError> {
        if !bytes.len().is_multiple_of(8) {
            return Err(ScanError::InvalidColumnBytes {
                len: bytes.len(),
                width: 8,
            });
        }

        #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
        {
            if std::arch::is_x86_feature_detected!("avx") {
                // Safety: AVX support is checked at runtime and the byte
                // slice length is validated as a multiple of 8.
                return Ok(unsafe { gt_filter_f64_bytes_avx(bytes, threshold) });
            }
        }

        Ok(bytes
            .chunks_exact(8)
            .enumerate()
            .filter_map(|(idx, chunk)| {
                let value = f64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes"));
                (value > threshold).then_some(idx)
            })
            .collect())
    }

    fn scalar_range_filter_f64(
        values: impl Iterator<Item = f64>,
        min: f64,
        max: f64,
    ) -> Vec<usize> {
        values
            .enumerate()
            .filter_map(|(idx, value)| (value >= min && value < max).then_some(idx))
            .collect()
    }

    fn scalar_gt_filter_f64(values: impl Iterator<Item = f64>, threshold: f64) -> Vec<usize> {
        values
            .enumerate()
            .filter_map(|(idx, value)| (value > threshold).then_some(idx))
            .collect()
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    #[target_feature(enable = "avx2")]
    unsafe fn range_filter_i64_bytes_avx2(bytes: &[u8], start: i64, end: i64) -> Vec<usize> {
        use std::arch::x86_64::{
            __m256i, _mm256_and_si256, _mm256_castsi256_pd, _mm256_cmpeq_epi64, _mm256_cmpgt_epi64,
            _mm256_loadu_si256, _mm256_movemask_pd, _mm256_set1_epi64x,
        };

        let lanes = bytes.len() / 8;
        let mut output = Vec::new();
        let mut idx = 0usize;
        let end_v = _mm256_set1_epi64x(end);
        let start_minus_one = start.checked_sub(1).map(|value| _mm256_set1_epi64x(value));

        while idx + 4 <= lanes {
            // Safety: `idx + 4 <= lanes`, `lanes == bytes.len() / 8`, and
            // the caller validated that `bytes.len()` is a multiple of 8.
            let ptr = unsafe { bytes.as_ptr().add(idx * 8) }.cast::<__m256i>();
            // Safety: `ptr` points at 32 readable bytes and unaligned loads
            // are explicitly supported by `_mm256_loadu_si256`.
            let values = unsafe { _mm256_loadu_si256(ptr) };
            let lt_end = _mm256_cmpgt_epi64(end_v, values);
            let ge_start = match start_minus_one {
                Some(start_v) => _mm256_cmpgt_epi64(values, start_v),
                None => _mm256_cmpeq_epi64(values, values),
            };
            let both = _mm256_and_si256(ge_start, lt_end);
            let mask = _mm256_movemask_pd(_mm256_castsi256_pd(both)) as u32;
            push_mask_indices(mask, idx, &mut output);
            idx += 4;
        }

        for lane in idx..lanes {
            let start_byte = lane * 8;
            let value = i64::from_le_bytes(
                bytes[start_byte..start_byte + 8]
                    .try_into()
                    .expect("slice is 8 bytes"),
            );
            if value >= start && value < end {
                output.push(lane);
            }
        }
        output
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    #[target_feature(enable = "avx")]
    unsafe fn range_filter_f64_values_avx(values: &[f64], min: f64, max: f64) -> Vec<usize> {
        use std::arch::x86_64::{
            _mm256_and_pd, _mm256_cmp_pd, _mm256_loadu_pd, _mm256_movemask_pd, _mm256_set1_pd,
            _CMP_GE_OQ, _CMP_LT_OQ,
        };

        let mut output = Vec::new();
        let mut idx = 0usize;
        let min_v = _mm256_set1_pd(min);
        let max_v = _mm256_set1_pd(max);
        while idx + 4 <= values.len() {
            // Safety: `idx + 4 <= values.len()`, so four initialized f64
            // values are readable. `_mm256_loadu_pd` permits unaligned input.
            let ptr = unsafe { values.as_ptr().add(idx) };
            let loaded = unsafe { _mm256_loadu_pd(ptr) };
            let ge_min = _mm256_cmp_pd(loaded, min_v, _CMP_GE_OQ);
            let lt_max = _mm256_cmp_pd(loaded, max_v, _CMP_LT_OQ);
            let mask = _mm256_movemask_pd(_mm256_and_pd(ge_min, lt_max)) as u32;
            push_mask_indices(mask, idx, &mut output);
            idx += 4;
        }

        for (offset, &value) in values[idx..].iter().enumerate() {
            if value >= min && value < max {
                output.push(idx + offset);
            }
        }
        output
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    #[target_feature(enable = "avx")]
    unsafe fn gt_filter_f64_values_avx(values: &[f64], threshold: f64) -> Vec<usize> {
        use std::arch::x86_64::{
            _mm256_cmp_pd, _mm256_loadu_pd, _mm256_movemask_pd, _mm256_set1_pd, _CMP_GT_OQ,
        };

        let mut output = Vec::new();
        let mut idx = 0usize;
        let threshold_v = _mm256_set1_pd(threshold);
        while idx + 4 <= values.len() {
            // Safety: `idx + 4 <= values.len()`, so four initialized f64
            // values are readable. `_mm256_loadu_pd` permits unaligned input.
            let ptr = unsafe { values.as_ptr().add(idx) };
            let loaded = unsafe { _mm256_loadu_pd(ptr) };
            let mask = _mm256_movemask_pd(_mm256_cmp_pd(loaded, threshold_v, _CMP_GT_OQ)) as u32;
            push_mask_indices(mask, idx, &mut output);
            idx += 4;
        }

        for (offset, &value) in values[idx..].iter().enumerate() {
            if value > threshold {
                output.push(idx + offset);
            }
        }
        output
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    #[target_feature(enable = "avx")]
    unsafe fn range_filter_f64_bytes_avx(bytes: &[u8], min: f64, max: f64) -> Vec<usize> {
        use std::arch::x86_64::{
            _mm256_and_pd, _mm256_cmp_pd, _mm256_loadu_pd, _mm256_movemask_pd, _mm256_set1_pd,
            _CMP_GE_OQ, _CMP_LT_OQ,
        };

        let lanes = bytes.len() / 8;
        let mut output = Vec::new();
        let mut idx = 0usize;
        let min_v = _mm256_set1_pd(min);
        let max_v = _mm256_set1_pd(max);
        while idx + 4 <= lanes {
            // Safety: `idx + 4 <= lanes`, `lanes == bytes.len() / 8`, and
            // the caller validated that `bytes.len()` is a multiple of 8.
            let ptr = unsafe { bytes.as_ptr().add(idx * 8) }.cast::<f64>();
            // Safety: `ptr` points at 32 readable bytes and unaligned loads
            // are explicitly supported by `_mm256_loadu_pd`.
            let loaded = unsafe { _mm256_loadu_pd(ptr) };
            let ge_min = _mm256_cmp_pd(loaded, min_v, _CMP_GE_OQ);
            let lt_max = _mm256_cmp_pd(loaded, max_v, _CMP_LT_OQ);
            let mask = _mm256_movemask_pd(_mm256_and_pd(ge_min, lt_max)) as u32;
            push_mask_indices(mask, idx, &mut output);
            idx += 4;
        }

        for lane in idx..lanes {
            let start_byte = lane * 8;
            let value = f64::from_le_bytes(
                bytes[start_byte..start_byte + 8]
                    .try_into()
                    .expect("slice is 8 bytes"),
            );
            if value >= min && value < max {
                output.push(lane);
            }
        }
        output
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    #[target_feature(enable = "avx")]
    unsafe fn gt_filter_f64_bytes_avx(bytes: &[u8], threshold: f64) -> Vec<usize> {
        use std::arch::x86_64::{
            _mm256_cmp_pd, _mm256_loadu_pd, _mm256_movemask_pd, _mm256_set1_pd, _CMP_GT_OQ,
        };

        let lanes = bytes.len() / 8;
        let mut output = Vec::new();
        let mut idx = 0usize;
        let threshold_v = _mm256_set1_pd(threshold);
        while idx + 4 <= lanes {
            // Safety: `idx + 4 <= lanes`, `lanes == bytes.len() / 8`, and
            // the caller validated that `bytes.len()` is a multiple of 8.
            let ptr = unsafe { bytes.as_ptr().add(idx * 8) }.cast::<f64>();
            // Safety: `ptr` points at 32 readable bytes and unaligned loads
            // are explicitly supported by `_mm256_loadu_pd`.
            let loaded = unsafe { _mm256_loadu_pd(ptr) };
            let mask = _mm256_movemask_pd(_mm256_cmp_pd(loaded, threshold_v, _CMP_GT_OQ)) as u32;
            push_mask_indices(mask, idx, &mut output);
            idx += 4;
        }

        for lane in idx..lanes {
            let start_byte = lane * 8;
            let value = f64::from_le_bytes(
                bytes[start_byte..start_byte + 8]
                    .try_into()
                    .expect("slice is 8 bytes"),
            );
            if value > threshold {
                output.push(lane);
            }
        }
        output
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    fn push_mask_indices(mut mask: u32, base: usize, output: &mut Vec<usize>) {
        while mask != 0 {
            let lane = mask.trailing_zeros() as usize;
            output.push(base + lane);
            mask &= mask - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::batch::{ColumnData, RowBatch};
    use crate::schema::{ColumnDef, DType, Schema};

    use super::{
        filter_indices, filter_scan_indices, range_filter_i64_mapped, ColumnPredicate, GtPredicate,
        RangePredicate, ScanResult,
    };

    fn batch() -> RowBatch {
        let schema = Schema::new(
            "ticks",
            vec![
                ColumnDef::new("timestamp", DType::Timestamp).unwrap(),
                ColumnDef::new("price", DType::F64).unwrap(),
            ],
        )
        .unwrap();
        RowBatch::builder(&schema)
            .column("timestamp", ColumnData::Timestamp(vec![1, 2, 3]))
            .column("price", ColumnData::F64(vec![10.0, 20.0, 30.0]))
            .build()
            .unwrap()
    }

    #[test]
    fn scan_result_wraps_batch_columns() {
        let result = ScanResult::from_batch(&batch());
        assert_eq!(result.row_count(), 3);
        assert_eq!(
            result.column("price").unwrap().as_f64().unwrap(),
            &[10.0, 20.0, 30.0]
        );
        assert!(!result.column("price").unwrap().has_mapped_chunks());
    }

    #[test]
    fn range_predicate_filters_indices() {
        let predicate = RangePredicate::new("price", 15.0, 30.0);
        assert_eq!(predicate.column(), "price");
        assert_eq!(filter_indices(&batch(), &predicate).unwrap(), vec![1]);
    }

    #[test]
    fn scan_result_filters_with_simd_range_path() {
        let result = ScanResult::from_batch(&batch());
        let predicate = RangePredicate::new("price", 10.0, 30.0);
        assert_eq!(
            filter_scan_indices(&result, &predicate).unwrap(),
            vec![0, 1]
        );
    }

    #[test]
    fn scan_result_filters_with_simd_greater_than_path() {
        let result = ScanResult::from_batch(&batch());
        let predicate = GtPredicate::new("price", 19.0);
        assert_eq!(
            filter_scan_indices(&result, &predicate).unwrap(),
            vec![1, 2]
        );
    }

    #[test]
    fn row_selection_compresses_identity_and_ranges() {
        assert!(matches!(
            super::RowSelection::from_indices(vec![0, 1, 2], 3),
            super::RowSelection::All { len: 3 }
        ));
        assert!(matches!(
            super::RowSelection::from_indices(vec![4, 5, 6], 10),
            super::RowSelection::Range { start: 4, len: 3 }
        ));
        assert!(matches!(
            super::RowSelection::from_indices(vec![1, 3, 4], 10),
            super::RowSelection::Indices(_)
        ));
    }

    #[test]
    fn i64_byte_filter_rejects_invalid_length() {
        let err = super::simd::range_filter_i64_bytes(&[1, 2, 3], 0, 10).unwrap_err();
        assert_eq!(
            err,
            super::ScanError::InvalidColumnBytes { len: 3, width: 8 }
        );
    }

    #[test]
    fn mapped_filter_symbol_is_reachable_for_tests() {
        let _ = range_filter_i64_mapped;
    }
}
