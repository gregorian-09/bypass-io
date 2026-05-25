use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bypass_db::{ColumnData, ColumnDef, DType, RowBatch, Schema, Table};
use bypass_io::{BypassConfig, UringBackend};
use clap::{Parser, Subcommand};
use tracing::{info, warn};

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.trace_json);
    if let Err(err) = run(cli) {
        eprintln!("{err}");
        std::process::exit(err.exit_code());
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Config { command } => run_config(command),
        Command::Bench { command } => run_bench(command),
    }
}

fn run_config(command: ConfigCommand) -> Result<(), CliError> {
    match command {
        ConfigCommand::Default { output } => {
            let text = BypassConfig::default().to_toml_string();
            if let Some(path) = output {
                fs::write(&path, text).map_err(CliError::Io)?;
                info!(
                    event = "config_default_written",
                    file = %path.display(),
                    "wrote default configuration"
                );
            } else {
                print!("{text}");
                info!(
                    event = "config_default_printed",
                    "printed default configuration"
                );
            }
            Ok(())
        }
        ConfigCommand::Validate { file } => {
            BypassConfig::load(&file)?;
            println!("config_ok file={}", file.display());
            info!(
                event = "config_validated",
                file = %file.display(),
                "validated configuration"
            );
            Ok(())
        }
    }
}

fn run_bench(command: BenchCommand) -> Result<(), CliError> {
    match command {
        BenchCommand::Uring {
            file,
            buf_size,
            depth,
            duration,
        } => bench_uring(file, buf_size, depth, duration),
        BenchCommand::Db {
            path,
            rows_per_batch,
            batches,
        } => bench_db(path, rows_per_batch, batches),
        BenchCommand::Spdk { pci, .. } => {
            warn!(
                event = "spdk_benchmark_unsupported",
                pci = %pci,
                "SPDK benchmark requested before native runtime support"
            );
            Err(CliError::Unsupported(
                "SPDK benchmarking requires the native SPDK runtime and hardware binding phase",
            ))
        }
        BenchCommand::Dpdk { pci, .. } => {
            warn!(
                event = "dpdk_benchmark_unsupported",
                pci = %pci,
                "DPDK benchmark requested before native runtime support"
            );
            Err(CliError::Unsupported(
                "DPDK benchmarking requires the native DPDK runtime and NIC binding phase",
            ))
        }
    }
}

fn bench_uring(
    file: PathBuf,
    buf_size: usize,
    _depth: usize,
    duration: Duration,
) -> Result<(), CliError> {
    let backend = UringBackend::new(256).map_err(CliError::Io)?;
    let file_handle = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&file)
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

    let elapsed = started.elapsed();
    info!(
        event = "benchmark_complete",
        benchmark = "uring_write",
        file = %file.display(),
        ops,
        bytes,
        elapsed_ms = elapsed.as_millis(),
        ops_per_sec = rate(ops, elapsed),
        mib_per_sec = mib_per_sec(bytes, elapsed),
        "completed io_uring write benchmark"
    );
    print_benchmark_result("uring_write", ops, bytes, elapsed);
    Ok(())
}

fn bench_db(path: PathBuf, rows_per_batch: usize, batches: usize) -> Result<(), CliError> {
    let schema = Schema::new(
        "trades",
        vec![
            ColumnDef::new("timestamp", DType::Timestamp)?,
            ColumnDef::new("symbol", DType::FixedStr(8))?,
            ColumnDef::new("price", DType::F64)?,
            ColumnDef::new("volume", DType::F64)?,
        ],
    )?;
    let mut table = Table::open(&path, schema.clone())?;
    let started = Instant::now();
    let mut total_rows = 0usize;

    for batch_no in 0..batches {
        let batch = make_batch(&schema, rows_per_batch, batch_no)?;
        total_rows = total_rows.saturating_add(table.append(&batch)?);
    }
    table.flush()?;

    let elapsed = started.elapsed();
    info!(
        event = "benchmark_complete",
        benchmark = "db_append",
        path = %path.display(),
        rows = total_rows,
        batches,
        elapsed_ms = elapsed.as_millis(),
        rows_per_sec = rate(total_rows, elapsed),
        "completed bypass-db append benchmark"
    );
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
        mib_per_sec(bytes, elapsed)
    );
}

fn rate(units: usize, elapsed: Duration) -> f64 {
    units as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
}

fn mib_per_sec(bytes: usize, elapsed: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(f64::EPSILON)
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let split = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    if split == 0 {
        return Err("duration must start with a number".to_string());
    }
    let count = value[..split]
        .parse::<u64>()
        .map_err(|_| "duration count must be an unsigned integer".to_string())?;
    match &value[split..] {
        "ms" => Ok(Duration::from_millis(count)),
        "s" | "" => Ok(Duration::from_secs(count)),
        "m" => Ok(Duration::from_secs(count.saturating_mul(60))),
        "h" => Ok(Duration::from_secs(count.saturating_mul(60 * 60))),
        suffix => Err(format!("unsupported duration suffix {suffix:?}")),
    }
}

fn init_tracing(json: bool) {
    if json {
        tracing_subscriber::fmt().json().init();
    }
}

/// `bypass-io` benchmark and configuration harness.
#[derive(Debug, Parser)]
#[command(name = "bypass-cli", version, about)]
struct Cli {
    /// Emit structured tracing events as JSON.
    #[arg(long, global = true)]
    trace_json: bool,
    /// Command to run.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate or validate runtime configuration.
    Config {
        /// Configuration command.
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run local benchmark harnesses.
    Bench {
        /// Benchmark command.
        #[command(subcommand)]
        command: BenchCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print or write a default `bypass-io.toml`.
    Default {
        /// Output file. Prints to stdout when omitted.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Validate a `bypass-io.toml` file.
    Validate {
        /// Configuration file path.
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum BenchCommand {
    /// Benchmark local `io_uring` write throughput.
    Uring {
        /// File to write.
        #[arg(long)]
        file: PathBuf,
        /// Write buffer size in bytes.
        #[arg(long, default_value_t = 4096)]
        buf_size: usize,
        /// Submission depth target for future async benchmark variants.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Benchmark duration, such as `500ms`, `10s`, `2m`, or `1h`.
        #[arg(long, default_value = "10s", value_parser = parse_duration)]
        duration: Duration,
    },
    /// Benchmark local `bypass-db` append throughput.
    Db {
        /// Table root path.
        #[arg(long)]
        path: PathBuf,
        /// Rows per generated batch.
        #[arg(long, default_value_t = 10_000)]
        rows_per_batch: usize,
        /// Number of batches to append.
        #[arg(long, default_value_t = 1_000)]
        batches: usize,
    },
    /// Placeholder for native SPDK benchmark support.
    Spdk {
        /// NVMe PCI BDF.
        #[arg(long)]
        pci: String,
        /// Read/write mode.
        #[arg(long, default_value = "write")]
        rw: String,
        /// Block size in bytes.
        #[arg(long, default_value_t = 4096)]
        block_size: usize,
        /// Queue depth.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Benchmark duration.
        #[arg(long, default_value = "30s", value_parser = parse_duration)]
        duration: Duration,
    },
    /// Placeholder for native DPDK benchmark support.
    Dpdk {
        /// NIC PCI BDF.
        #[arg(long)]
        pci: String,
        /// Packet mode.
        #[arg(long, default_value = "rx")]
        mode: String,
        /// Benchmark duration.
        #[arg(long, default_value = "10s", value_parser = parse_duration)]
        duration: Duration,
    },
}

#[derive(Debug)]
enum CliError {
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
            Self::Unsupported(_) => 3,
            Self::Io(_) | Self::Config(_) | Self::Schema(_) | Self::Batch(_) | Self::Table(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_duration;

    #[test]
    fn parse_duration_supports_common_suffixes() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("10s"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert!(parse_duration("bad").is_err());
    }
}
