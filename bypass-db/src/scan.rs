use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::batch::{ColumnData, RowBatch};

/// Result of a table scan.
#[derive(Clone, Debug, PartialEq)]
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

/// Column data returned by a scan.
#[derive(Clone, Debug, PartialEq)]
pub enum ScanColumn {
    /// `f64` values.
    F64(Arc<[f64]>),
    /// `i64` values.
    I64(Arc<[i64]>),
    /// Timestamp values in nanoseconds since Unix epoch.
    Timestamp(Arc<[i64]>),
    /// Fixed-width byte strings.
    FixedStr {
        width: usize,
        values: Arc<[Vec<u8>]>,
    },
}

impl ScanColumn {
    /// Borrow as `f64` values.
    #[must_use]
    pub fn as_f64(&self) -> Option<&[f64]> {
        match self {
            Self::F64(values) => Some(values),
            _ => None,
        }
    }

    /// Borrow as `i64` values.
    #[must_use]
    pub fn as_i64(&self) -> Option<&[i64]> {
        match self {
            Self::I64(values) | Self::Timestamp(values) => Some(values),
            _ => None,
        }
    }

    /// Borrow as timestamp values.
    #[must_use]
    pub fn as_timestamps(&self) -> Option<&[i64]> {
        match self {
            Self::Timestamp(values) => Some(values),
            _ => None,
        }
    }
}

impl From<ColumnData> for ScanColumn {
    fn from(value: ColumnData) -> Self {
        match value {
            ColumnData::F64(values) => Self::F64(values.into()),
            ColumnData::I64(values) => Self::I64(values.into()),
            ColumnData::Timestamp(values) => Self::Timestamp(values.into()),
            ColumnData::FixedStr { width, values } => Self::FixedStr {
                width,
                values: values.into(),
            },
        }
    }
}

/// Predicate over one table column.
pub trait ColumnPredicate: Send + Sync {
    /// Column targeted by this predicate.
    fn column(&self) -> &str;

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

/// Scan error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanError {
    /// Predicate referenced an unknown column.
    UnknownColumn {
        /// Unknown column name.
        column: String,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownColumn { column } => write!(f, "unknown scan column {column}"),
        }
    }
}

impl Error for ScanError {}

#[cfg(test)]
mod tests {
    use crate::batch::{ColumnData, RowBatch};
    use crate::schema::{ColumnDef, DType, Schema};

    use super::{filter_indices, ColumnPredicate, RangePredicate, ScanResult};

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
    }

    #[test]
    fn range_predicate_filters_indices() {
        let predicate = RangePredicate::new("price", 15.0, 30.0);
        assert_eq!(predicate.column(), "price");
        assert_eq!(filter_indices(&batch(), &predicate).unwrap(), vec![1]);
    }
}
