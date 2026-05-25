use std::collections::HashSet;
use std::error::Error;
use std::fmt;

/// Logical type of a table column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DType {
    /// 64-bit floating point value.
    F64,
    /// 64-bit signed integer value.
    I64,
    /// Nanoseconds since Unix epoch stored as `i64`.
    Timestamp,
    /// Fixed-width byte string.
    FixedStr(usize),
}

impl DType {
    /// Return true when the type is a fixed-width string.
    #[must_use]
    pub const fn is_fixed_str(&self) -> bool {
        matches!(self, Self::FixedStr(_))
    }
}

/// Column definition in a [`Schema`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnDef {
    name: String,
    dtype: DType,
}

impl ColumnDef {
    /// Create a column definition.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::EmptyColumnName`] for an empty name or
    /// [`SchemaError::InvalidFixedStringWidth`] for `FixedStr(0)`.
    pub fn new(name: impl Into<String>, dtype: DType) -> Result<Self, SchemaError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SchemaError::EmptyColumnName);
        }
        if matches!(dtype, DType::FixedStr(0)) {
            return Err(SchemaError::InvalidFixedStringWidth { column: name });
        }
        Ok(Self { name, dtype })
    }

    /// Column name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Column type.
    #[must_use]
    pub fn dtype(&self) -> &DType {
        &self.dtype
    }
}

/// Table schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Schema {
    name: String,
    columns: Vec<ColumnDef>,
    timestamp_column: usize,
}

impl Schema {
    /// Create a schema.
    ///
    /// # Errors
    ///
    /// Returns an error when the table name is empty, there are no columns,
    /// column names are duplicated, or there is not exactly one timestamp
    /// column.
    pub fn new(name: impl Into<String>, columns: Vec<ColumnDef>) -> Result<Self, SchemaError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SchemaError::EmptyTableName);
        }
        if columns.is_empty() {
            return Err(SchemaError::NoColumns);
        }

        let mut seen = HashSet::with_capacity(columns.len());
        let mut timestamp_column = None;
        for (idx, column) in columns.iter().enumerate() {
            if !seen.insert(column.name.as_str()) {
                return Err(SchemaError::DuplicateColumn {
                    column: column.name.clone(),
                });
            }
            if column.dtype == DType::Timestamp && timestamp_column.replace(idx).is_some() {
                return Err(SchemaError::MultipleTimestampColumns);
            }
        }
        let timestamp_column = timestamp_column.ok_or(SchemaError::MissingTimestampColumn)?;

        Ok(Self {
            name,
            columns,
            timestamp_column,
        })
    }

    /// Table name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Ordered column definitions.
    #[must_use]
    pub fn columns(&self) -> &[ColumnDef] {
        &self.columns
    }

    /// Timestamp column definition.
    #[must_use]
    pub fn timestamp_column(&self) -> &ColumnDef {
        &self.columns[self.timestamp_column]
    }

    /// Find a column by name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|column| column.name == name)
    }

    /// Return the index of a column by name.
    #[must_use]
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column.name == name)
    }
}

/// Schema validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaError {
    /// Table name is empty.
    EmptyTableName,
    /// Column name is empty.
    EmptyColumnName,
    /// Schema has no columns.
    NoColumns,
    /// Column appears more than once.
    DuplicateColumn {
        /// Duplicated column.
        column: String,
    },
    /// Fixed-width string column has width zero.
    InvalidFixedStringWidth {
        /// Column with invalid width.
        column: String,
    },
    /// Schema has no timestamp column.
    MissingTimestampColumn,
    /// Schema has more than one timestamp column.
    MultipleTimestampColumns,
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTableName => write!(f, "table name must not be empty"),
            Self::EmptyColumnName => write!(f, "column name must not be empty"),
            Self::NoColumns => write!(f, "schema must contain at least one column"),
            Self::DuplicateColumn { column } => write!(f, "duplicate column {column}"),
            Self::InvalidFixedStringWidth { column } => {
                write!(f, "fixed string column {column} must have non-zero width")
            }
            Self::MissingTimestampColumn => write!(f, "schema must contain a timestamp column"),
            Self::MultipleTimestampColumns => {
                write!(f, "schema must contain exactly one timestamp column")
            }
        }
    }
}

impl Error for SchemaError {}

#[cfg(test)]
mod tests {
    use super::{ColumnDef, DType, Schema, SchemaError};

    #[test]
    fn schema_requires_exactly_one_timestamp_column() {
        let err =
            Schema::new("ticks", vec![ColumnDef::new("price", DType::F64).unwrap()]).unwrap_err();
        assert_eq!(err, SchemaError::MissingTimestampColumn);

        let err = Schema::new(
            "ticks",
            vec![
                ColumnDef::new("ts", DType::Timestamp).unwrap(),
                ColumnDef::new("other_ts", DType::Timestamp).unwrap(),
            ],
        )
        .unwrap_err();
        assert_eq!(err, SchemaError::MultipleTimestampColumns);
    }

    #[test]
    fn schema_rejects_duplicate_columns() {
        let err = Schema::new(
            "ticks",
            vec![
                ColumnDef::new("ts", DType::Timestamp).unwrap(),
                ColumnDef::new("ts", DType::I64).unwrap(),
            ],
        )
        .unwrap_err();
        assert_eq!(
            err,
            SchemaError::DuplicateColumn {
                column: "ts".to_string()
            }
        );
    }
}
