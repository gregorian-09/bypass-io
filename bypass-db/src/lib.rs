#![doc = include_str!("../README.md")]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod batch;
pub mod scan;
pub mod schema;
pub mod table;
pub mod wal;

pub use batch::{BatchError, ColumnData, RowBatch, RowBatchBuilder};
pub use scan::{
    ColumnPredicate, GtPredicate, RangePredicate, ScalarPredicate, ScanColumn, ScanError,
    ScanResult,
};
pub use schema::{ColumnDef, DType, Schema, SchemaError};
pub use table::{Table, TableError};
pub use wal::{WalError, WalReader, WalRecord, WalWriter, WAL_MAGIC};
