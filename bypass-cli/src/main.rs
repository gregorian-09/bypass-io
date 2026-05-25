use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bypass_db::{ColumnData, ColumnDef, DType, RowBatch, Schema, Table};
use bypass_io::{BypassConfig, UringBackend};

fn main() {
    if let Err(err) = run(env::args().skip(1).collect()) {
        eprintln!("{err}");
        std::process::exit(err.exit_code());
    }
}

fn run(args: Vec<String>) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("config") => run_config(&args[1..]),
        Some("bench") => run_bench(&args[1..]),
        Some("-h" | "--help") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!("unknown command {other}"))),
    }
}

fn run_config(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("default") => {
            let output = optional_value(args, "--output")?;
            let text = BypassConfig::default().to_toml_string();
            if let Some(path) = output {
                fs::write(path, text).map_err(CliError::Io)?;
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Some("validate") => {
            let file = required_value(args, "--file")?;
            BypassConfig::load(&file)?;
            println!("config_ok file={}", file.display());
            Ok(())
        }
        Some("-h" | "--help") | None => {
            print_config_help();
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!("unknown config command {other}"))),
    }
}

fn run_bench(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("uring") => bench_uring(args),
        Some("db") => bench_db(args),
        Some("spdk") => Err(CliError::Unsupported(
            "SPDK benchmarking requires the native SPDK runtime and hardware binding phase",
        )),
        Some("dpdk") => Err(CliError::Unsupported(
            "DPDK benchmarking requires the native DPDK runtime and NIC binding phase",
        )),
        Some("-h" | "--help") | None => {
            print_bench_help();
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!("unknown benchmark {other}"))),
    }
}

fn bench_uring(args: &[String]) -> Result<(), CliError> {
    let file = required_value(args, "--file")?;
    let buf_size = parse_usize_arg(args, "--buf-size", 4096)?;
    let _depth = parse_usize_arg(args, "--depth", 1)?;
    let duration = parse_duration_arg(args, "--duration", Duration::from_secs(10))?;
    let backend = UringBackend::new(256).map_err(CliError::Io)?;
    let file_handle = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(file)
        .map_err(CliError::Io)?;
    let buffer = vec![0x5a; buf_size];
    let started = Instant::now();
    let mut offset = 0u64;
    let mut ops = 0usize;
    let mut bytes = 0usize;

    while started.elapsed() < duration {
        let written = backend
            .write_at(file_handle.as_raw_fd(), &buffer, offset)
            .map_err(CliError::Io)?;
        offset = offset.saturating_add(written as u64);
        ops = ops.saturating_add(1);
        bytes = bytes.saturating_add(written);
    }
    backend
        .fsync(file_handle.as_raw_fd())
        .map_err(CliError::Io)?;

    print_benchmark_result("uring_write", ops, bytes, started.elapsed());
    Ok(())
}

fn bench_db(args: &[String]) -> Result<(), CliError> {
    let path = required_value(args, "--path")?;
    let rows_per_batch = parse_usize_arg(args, "--rows-per-batch", 10_000)?;
    let batches = parse_usize_arg(args, "--batches", 1_000)?;
    let schema = Schema::new(
        "trades",
        vec![
            ColumnDef::new("timestamp", DType::Timestamp)?,
            ColumnDef::new("symbol", DType::FixedStr(8))?,
            ColumnDef::new("price", DType::F64)?,
            ColumnDef::new("volume", DType::F64)?,
        ],
    )?;
    let mut table = Table::open(path, schema.clone())?;
    let started = Instant::now();
    let mut total_rows = 0usize;

    for batch_no in 0..batches {
        let batch = make_batch(&schema, rows_per_batch, batch_no)?;
        total_rows = total_rows.saturating_add(table.append(&batch)?);
    }
    table.flush()?;

    let elapsed = started.elapsed();
    println!(
        "benchmark=db_append rows={} batches={} elapsed_ms={} rows_per_sec={:.2}",
        total_rows,
        batches,
        elapsed.as_millis(),
        rate(total_rows, elapsed)
    );
    Ok(())
}

fn make_batch(schema: &Schema, rows: usize, batch_no: usize) -> Result<RowBatch, CliError> {
    let base = (batch_no * rows) as i64;
    let timestamps = (0..rows).map(|idx| base + idx as i64).collect::<Vec<_>>();
    let symbols = (0..rows)
        .map(|idx| {
            if idx % 2 == 0 {
                b"EURUSD  ".to_vec()
            } else {
                b"BTCUSD  ".to_vec()
            }
        })
        .collect::<Vec<_>>();
    let prices = (0..rows)
        .map(|idx| 100.0 + ((idx % 10_000) as f64 * 0.01))
        .collect::<Vec<_>>();
    let volumes = (0..rows)
        .map(|idx| 1.0 + (idx % 100) as f64)
        .collect::<Vec<_>>();

    Ok(RowBatch::builder(schema)
        .column("timestamp", ColumnData::Timestamp(timestamps))
        .column(
            "symbol",
            ColumnData::FixedStr {
                width: 8,
                values: symbols,
            },
        )
        .column("price", ColumnData::F64(prices))
        .column("volume", ColumnData::F64(volumes))
        .build()?)
}

fn print_benchmark_result(name: &str, ops: usize, bytes: usize, elapsed: Duration) {
    println!(
        "benchmark={} ops={} bytes={} elapsed_ms={} ops_per_sec={:.2} mib_per_sec={:.2}",
        name,
        ops,
        bytes,
        elapsed.as_millis(),
        rate(ops, elapsed),
        bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(f64::EPSILON)
    );
}

fn rate(units: usize, elapsed: Duration) -> f64 {
    units as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
}

fn required_value(args: &[String], flag: &str) -> Result<PathBuf, CliError> {
    optional_value(args, flag)?
        .map(PathBuf::from)
        .ok_or_else(|| CliError::Usage(format!("missing required {flag}")))
}

fn optional_value<'a>(args: &'a [String], flag: &str) -> Result<Option<&'a str>, CliError> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return Ok(None);
    };
    args.get(index + 1)
        .map(String::as_str)
        .map(Some)
        .ok_or_else(|| CliError::Usage(format!("missing value for {flag}")))
}

fn parse_usize_arg(args: &[String], flag: &str, default: usize) -> Result<usize, CliError> {
    let Some(value) = optional_value(args, flag)? else {
        return Ok(default);
    };
    value
        .parse()
        .map_err(|_| CliError::Usage(format!("{flag} must be an unsigned integer")))
}

fn parse_duration_arg(
    args: &[String],
    flag: &str,
    default: Duration,
) -> Result<Duration, CliError> {
    let Some(value) = optional_value(args, flag)? else {
        return Ok(default);
    };
    parse_duration(value).ok_or_else(|| {
        CliError::Usage(format!(
            "{flag} must be a duration like 500ms, 10s, 2m, or 1h"
        ))
    })
}

fn parse_duration(value: &str) -> Option<Duration> {
    let split = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    if split == 0 {
        return None;
    }
    let count = value[..split].parse::<u64>().ok()?;
    match &value[split..] {
        "ms" => Some(Duration::from_millis(count)),
        "s" | "" => Some(Duration::from_secs(count)),
        "m" => Some(Duration::from_secs(count.saturating_mul(60))),
        "h" => Some(Duration::from_secs(count.saturating_mul(60 * 60))),
        _ => None,
    }
}

fn print_help() {
    println!("usage:");
    print_config_help();
    print_bench_help();
}

fn print_config_help() {
    println!("  bypass-cli config default [--output bypass-io.toml]");
    println!("  bypass-cli config validate --file bypass-io.toml");
}

fn print_bench_help() {
    println!(
        "  bypass-cli bench uring --file /tmp/test.bin [--buf-size 4096] [--depth 1] [--duration 10s]"
    );
    println!(
        "  bypass-cli bench db --path /tmp/bypass-db [--rows-per-batch 10000] [--batches 1000]"
    );
    println!("  bypass-cli bench spdk --pci 0000:01:00.0");
    println!("  bypass-cli bench dpdk --pci 0000:02:00.0");
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Unsupported(&'static str),
    Io(io::Error),
    Config(bypass_io::ConfigError),
    Schema(bypass_db::schema::SchemaError),
    Batch(bypass_db::batch::BatchError),
    Table(bypass_db::table::TableError),
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Unsupported(_) => 3,
            Self::Io(_) | Self::Config(_) | Self::Schema(_) | Self::Batch(_) | Self::Table(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "usage error: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported benchmark: {message}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Config(err) => write!(f, "{err}"),
            Self::Schema(err) => write!(f, "{err}"),
            Self::Batch(err) => write!(f, "{err}"),
            Self::Table(err) => write!(f, "{err}"),
        }
    }
}

impl Error for CliError {}

impl From<io::Error> for CliError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<bypass_io::ConfigError> for CliError {
    fn from(value: bypass_io::ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<bypass_db::schema::SchemaError> for CliError {
    fn from(value: bypass_db::schema::SchemaError) -> Self {
        Self::Schema(value)
    }
}

impl From<bypass_db::batch::BatchError> for CliError {
    fn from(value: bypass_db::batch::BatchError) -> Self {
        Self::Batch(value)
    }
}

impl From<bypass_db::table::TableError> for CliError {
    fn from(value: bypass_db::table::TableError) -> Self {
        Self::Table(value)
    }
}

#[allow(dead_code)]
fn path_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_duration;

    #[test]
    fn parse_duration_supports_common_suffixes() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("bad"), None);
    }
}
