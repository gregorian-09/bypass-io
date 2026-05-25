use std::error::Error;
use std::fmt;

use crate::schema::{DType, Schema};

/// Column data for one [`RowBatch`].
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnData {
    /// `f64` values.
    F64(Vec<f64>),
    /// `i64` values.
    I64(Vec<i64>),
    /// Timestamp values in nanoseconds since Unix epoch.
    Timestamp(Vec<i64>),
    /// Fixed-width byte strings.
    FixedStr { width: usize, values: Vec<Vec<u8>> },
}

impl ColumnData {
    /// Number of rows in the column.
    #[must_use]
    pub fn row_count(&self) -> usize {
        match self {
            Self::F64(values) => values.len(),
            Self::I64(values) | Self::Timestamp(values) => values.len(),
            Self::FixedStr { values, .. } => values.len(),
        }
    }

    /// Return true when this data matches a schema type.
    #[must_use]
    pub fn matches_dtype(&self, dtype: &DType) -> bool {
        matches!(
            (self, dtype),
            (Self::F64(_), DType::F64)
                | (Self::I64(_), DType::I64)
                | (Self::Timestamp(_), DType::Timestamp)
        ) || matches!((self, dtype), (Self::FixedStr { width, .. }, DType::FixedStr(expected)) if width == expected)
    }

    /// Append another same-typed column to this column.
    ///
    /// # Errors
    ///
    /// Returns [`BatchError::TypeMismatch`] when the column variants differ.
    pub fn append(&mut self, other: &Self) -> Result<(), BatchError> {
        match (self, other) {
            (Self::F64(left), Self::F64(right)) => left.extend_from_slice(right),
            (Self::I64(left), Self::I64(right))
            | (Self::Timestamp(left), Self::Timestamp(right)) => left.extend_from_slice(right),
            (
                Self::FixedStr {
                    width: left_width,
                    values: left,
                },
                Self::FixedStr {
                    width: right_width,
                    values: right,
                },
            ) if left_width == right_width => left.extend(right.iter().cloned()),
            _ => return Err(BatchError::TypeMismatch),
        }
        Ok(())
    }

    pub(crate) fn take_indices(&self, indices: &[usize]) -> Self {
        match self {
            Self::F64(values) => Self::F64(indices.iter().map(|&idx| values[idx]).collect()),
            Self::I64(values) => Self::I64(indices.iter().map(|&idx| values[idx]).collect()),
            Self::Timestamp(values) => {
                Self::Timestamp(indices.iter().map(|&idx| values[idx]).collect())
            }
            Self::FixedStr { width, values } => Self::FixedStr {
                width: *width,
                values: indices.iter().map(|&idx| values[idx].clone()).collect(),
            },
        }
    }

    pub(crate) fn timestamp_values(&self) -> Option<&[i64]> {
        match self {
            Self::Timestamp(values) => Some(values),
            _ => None,
        }
    }
}

/// Columnar batch of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct RowBatch {
    columns: Vec<(String, ColumnData)>,
    row_count: usize,
}

impl RowBatch {
    /// Start building a row batch for `schema`.
    #[must_use]
    pub fn builder(schema: &Schema) -> RowBatchBuilder<'_> {
        RowBatchBuilder {
            schema,
            columns: Vec::new(),
        }
    }

    pub(crate) fn empty(schema: &Schema) -> Self {
        let columns = schema
            .columns()
            .iter()
            .map(|column| {
                let data = match column.dtype() {
                    DType::F64 => ColumnData::F64(Vec::new()),
                    DType::I64 => ColumnData::I64(Vec::new()),
                    DType::Timestamp => ColumnData::Timestamp(Vec::new()),
                    DType::FixedStr(width) => ColumnData::FixedStr {
                        width: *width,
                        values: Vec::new(),
                    },
                };
                (column.name().to_string(), data)
            })
            .collect();
        Self {
            columns,
            row_count: 0,
        }
    }

    pub(crate) fn from_parts(columns: Vec<(String, ColumnData)>, row_count: usize) -> Self {
        Self { columns, row_count }
    }

    /// Number of rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Ordered column data.
    #[must_use]
    pub fn columns(&self) -> &[(String, ColumnData)] {
        &self.columns
    }

    /// Return a column by name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnData> {
        self.columns
            .iter()
            .find_map(|(column, data)| (column == name).then_some(data))
    }

    pub(crate) fn append(&mut self, other: &Self) -> Result<(), BatchError> {
        if self.columns.len() != other.columns.len() {
            return Err(BatchError::SchemaMismatch);
        }
        for ((left_name, left), (right_name, right)) in
            self.columns.iter_mut().zip(other.columns.iter())
        {
            if left_name != right_name {
                return Err(BatchError::SchemaMismatch);
            }
            left.append(right)?;
        }
        self.row_count = self
            .row_count
            .checked_add(other.row_count)
            .ok_or(BatchError::RowCountOverflow)?;
        Ok(())
    }

    pub(crate) fn take_indices(&self, indices: &[usize]) -> Self {
        let columns = self
            .columns
            .iter()
            .map(|(name, data)| (name.clone(), data.take_indices(indices)))
            .collect();
        Self {
            columns,
            row_count: indices.len(),
        }
    }
}

/// Builder for [`RowBatch`].
#[derive(Debug)]
pub struct RowBatchBuilder<'a> {
    schema: &'a Schema,
    columns: Vec<(String, ColumnData)>,
}

impl<'a> RowBatchBuilder<'a> {
    /// Add a column to the batch.
    #[must_use]
    pub fn column(mut self, name: impl Into<String>, data: ColumnData) -> Self {
        self.columns.push((name.into(), data));
        self
    }

    /// Validate and build the batch.
    ///
    /// # Errors
    ///
    /// Returns an error when columns do not match the schema exactly, types do
    /// not match, fixed string widths are invalid, or row counts differ.
    pub fn build(self) -> Result<RowBatch, BatchError> {
        if self.columns.len() != self.schema.columns().len() {
            return Err(BatchError::SchemaMismatch);
        }

        let mut ordered = Vec::with_capacity(self.columns.len());
        let mut row_count = None;

        for column in self.schema.columns() {
            let Some((_, data)) = self.columns.iter().find(|(name, _)| name == column.name())
            else {
                return Err(BatchError::MissingColumn {
                    column: column.name().to_string(),
                });
            };
            if !data.matches_dtype(column.dtype()) {
                return Err(BatchError::ColumnTypeMismatch {
                    column: column.name().to_string(),
                    expected: column.dtype().clone(),
                });
            }
            validate_fixed_str_width(column.name(), data)?;
            match row_count {
                Some(expected) if expected != data.row_count() => {
                    return Err(BatchError::RowCountMismatch {
                        column: column.name().to_string(),
                        expected,
                        actual: data.row_count(),
                    });
                }
                None => row_count = Some(data.row_count()),
                _ => {}
            }
            ordered.push((column.name().to_string(), data.clone()));
        }

        Ok(RowBatch {
            columns: ordered,
            row_count: row_count.unwrap_or(0),
        })
    }
}

fn validate_fixed_str_width(column: &str, data: &ColumnData) -> Result<(), BatchError> {
    let ColumnData::FixedStr { width, values } = data else {
        return Ok(());
    };
    if values.iter().any(|value| value.len() != *width) {
        return Err(BatchError::FixedStringWidthMismatch {
            column: column.to_string(),
            width: *width,
        });
    }
    Ok(())
}

/// Row-batch validation error.
#[derive(Clone, Debug, PartialEq)]
pub enum BatchError {
    /// Batch does not match schema.
    SchemaMismatch,
    /// Column is missing.
    MissingColumn {
        /// Missing column.
        column: String,
    },
    /// Column type does not match schema.
    ColumnTypeMismatch {
        /// Column name.
        column: String,
        /// Expected type.
        expected: DType,
    },
    /// Row counts are inconsistent.
    RowCountMismatch {
        /// Column with mismatched count.
        column: String,
        /// Expected row count.
        expected: usize,
        /// Actual row count.
        actual: usize,
    },
    /// Fixed string data value did not match the declared width.
    FixedStringWidthMismatch {
        /// Column name.
        column: String,
        /// Expected width.
        width: usize,
    },
    /// Columns have different variants.
    TypeMismatch,
    /// Row count overflowed usize.
    RowCountOverflow,
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaMismatch => write!(f, "row batch does not match schema"),
            Self::MissingColumn { column } => write!(f, "missing column {column}"),
            Self::ColumnTypeMismatch { column, expected } => {
                write!(
                    f,
                    "column {column} does not match expected type {expected:?}"
                )
            }
            Self::RowCountMismatch {
                column,
                expected,
                actual,
            } => write!(
                f,
                "column {column} has row count {actual}, expected {expected}"
            ),
            Self::FixedStringWidthMismatch { column, width } => {
                write!(f, "column {column} contains value not {width} bytes wide")
            }
            Self::TypeMismatch => write!(f, "column data variants do not match"),
            Self::RowCountOverflow => write!(f, "row count overflow"),
        }
    }
}

impl Error for BatchError {}

#[cfg(test)]
mod tests {
    use crate::schema::{ColumnDef, DType, Schema};

    use super::{BatchError, ColumnData, RowBatch};

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

    #[test]
    fn builder_orders_columns_by_schema() {
        let schema = schema();
        let batch = RowBatch::builder(&schema)
            .column("price", ColumnData::F64(vec![10.0, 11.0]))
            .column("timestamp", ColumnData::Timestamp(vec![1, 2]))
            .column(
                "symbol",
                ColumnData::FixedStr {
                    width: 4,
                    values: vec![b"MSFT".to_vec(), b"AAPL".to_vec()],
                },
            )
            .build()
            .unwrap();

        assert_eq!(batch.row_count(), 2);
        assert_eq!(batch.columns()[0].0, "timestamp");
        assert_eq!(batch.columns()[1].0, "price");
        assert_eq!(batch.columns()[2].0, "symbol");
    }

    #[test]
    fn builder_rejects_row_count_mismatch() {
        let schema = schema();
        let err = RowBatch::builder(&schema)
            .column("timestamp", ColumnData::Timestamp(vec![1]))
            .column("price", ColumnData::F64(vec![10.0, 11.0]))
            .column(
                "symbol",
                ColumnData::FixedStr {
                    width: 4,
                    values: vec![b"MSFT".to_vec()],
                },
            )
            .build()
            .unwrap_err();

        assert_eq!(
            err,
            BatchError::RowCountMismatch {
                column: "price".to_string(),
                expected: 1,
                actual: 2
            }
        );
    }
}
