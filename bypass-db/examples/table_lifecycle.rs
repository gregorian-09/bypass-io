use std::error::Error;
use std::fs;

use bypass_db::{ColumnData, ColumnDef, DType, RowBatch, Schema, Table};

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::temp_dir().join(format!("bypass-db-table-example-{}", std::process::id()));
    fs::remove_dir_all(&path).ok();

    let schema = Schema::new(
        "trades",
        vec![
            ColumnDef::new("timestamp", DType::Timestamp)?,
            ColumnDef::new("symbol", DType::FixedStr(8))?,
            ColumnDef::new("price", DType::F64)?,
            ColumnDef::new("volume", DType::F64)?,
        ],
    )?;

    let batch = RowBatch::builder(&schema)
        .column("timestamp", ColumnData::Timestamp(vec![1, 2, 3, 4]))
        .column(
            "symbol",
            ColumnData::FixedStr {
                width: 8,
                values: vec![
                    b"EURUSD  ".to_vec(),
                    b"EURUSD  ".to_vec(),
                    b"BTCUSD  ".to_vec(),
                    b"BTCUSD  ".to_vec(),
                ],
            },
        )
        .column("price", ColumnData::F64(vec![101.0, 102.5, 99.5, 110.0]))
        .column("volume", ColumnData::F64(vec![10.0, 12.0, 1.5, 2.0]))
        .build()?;

    let mut table = Table::open(&path, schema)?;
    table.append(&batch)?;
    table.flush()?;

    let scan = table.scan_time_range(2, 4)?;
    let segment_ids = table
        .manifest()
        .sealed_segments
        .iter()
        .map(|segment| segment.id)
        .collect::<Vec<_>>();
    table.compact(&segment_ids)?;

    println!(
        "table_example path={} scanned_rows={} sealed_segments={}",
        path.display(),
        scan.row_count(),
        table.sealed_segment_count()
    );

    fs::remove_dir_all(&path).ok();
    Ok(())
}
