use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bypass_db::{ColumnData, ColumnDef, DType, RangePredicate, RowBatch, Schema, Table};
use bypass_io::{BypassConfig, DpdkBackend, SpdkBackend, UringBackend};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
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
        Command::Doctor { command } => run_doctor(command),
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
            history,
        } => bench_uring(file, buf_size, depth, duration, history.as_deref()),
        BenchCommand::Db {
            path,
            rows_per_batch,
            batches,
            scan_iterations,
            compact,
            history,
        } => bench_db(
            path,
            rows_per_batch,
            batches,
            scan_iterations,
            compact,
            history.as_deref(),
        ),
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

fn run_doctor(command: DoctorCommand) -> Result<(), CliError> {
    match command {
        DoctorCommand::Native {
            spdk_pci,
            dpdk_pci,
            require_hugepages,
        } => doctor_native(spdk_pci.as_deref(), dpdk_pci.as_deref(), require_hugepages),
    }
}

fn doctor_native(
    spdk_pci: Option<&str>,
    dpdk_pci: Option<&str>,
    require_hugepages: bool,
) -> Result<(), CliError> {
    let spdk = SpdkBackend::native_status();
    let dpdk = DpdkBackend::native_status();
    println!(
        "native_status backend=spdk linked={} detail={:?}",
        spdk.linked, spdk.detail
    );
    println!(
        "native_status backend=dpdk linked={} detail={:?}",
        dpdk.linked, dpdk.detail
    );

    let checks = [
        kernel_check(),
        hugepage_check(require_hugepages),
        path_check(
            "vfio",
            Path::new("/dev/vfio/vfio"),
            CheckState::Warn,
            "/dev/vfio/vfio is present",
            "/dev/vfio/vfio is not present; VFIO-bound device tests will not run",
        ),
        path_check(
            "vfio_pci",
            Path::new("/sys/bus/pci/drivers/vfio-pci"),
            CheckState::Warn,
            "vfio-pci driver is visible in sysfs",
            "vfio-pci driver is not visible in sysfs",
        ),
        pci_check("spdk_pci", spdk_pci),
        pci_check("dpdk_pci", dpdk_pci),
    ];

    let failures = checks
        .iter()
        .inspect(|check| {
            println!(
                "host_check check={} status={} detail={:?}",
                check.name,
                check.state.as_str(),
                check.detail
            );
        })
        .filter(|check| check.state == CheckState::Fail)
        .count();

    if failures == 0 {
        println!("doctor_summary status=ok required_failures=0");
        Ok(())
    } else {
        println!("doctor_summary status=fail required_failures={failures}");
        Err(CliError::DoctorFailed { failures })
    }
}

fn bench_uring(
    file: PathBuf,
    buf_size: usize,
    _depth: usize,
    duration: Duration,
    history: Option<&Path>,
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
    record_benchmark(
        history,
        BenchRecord::new("uring_write", "ops", ops, elapsed)
            .with_bytes(bytes)
            .with_context("buf_size", buf_size),
    )?;
    Ok(())
}

fn bench_db(
    path: PathBuf,
    rows_per_batch: usize,
    batches: usize,
    scan_iterations: usize,
    compact: bool,
    history: Option<&Path>,
) -> Result<(), CliError> {
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
    record_benchmark(
        history,
        BenchRecord::new("db_append", "rows", total_rows, elapsed)
            .with_context("rows_per_batch", rows_per_batch)
            .with_context("batches", batches),
    )?;

    let scan_start = Instant::now();
    let mut scanned_rows = 0usize;
    for _ in 0..scan_iterations {
        let result = table.scan_time_range(0, total_rows as i64)?;
        scanned_rows = scanned_rows.saturating_add(result.row_count());
    }
    let scan_elapsed = scan_start.elapsed();
    info!(
        event = "benchmark_complete",
        benchmark = "db_scan_time_range",
        path = %path.display(),
        rows = scanned_rows,
        iterations = scan_iterations,
        elapsed_ms = scan_elapsed.as_millis(),
        rows_per_sec = rate(scanned_rows, scan_elapsed),
        "completed bypass-db mmap time-range scan benchmark"
    );
    println!(
        "benchmark=db_scan_time_range rows={} iterations={} elapsed_ms={} rows_per_sec={:.2}",
        scanned_rows,
        scan_iterations,
        scan_elapsed.as_millis(),
        rate(scanned_rows, scan_elapsed)
    );
    record_benchmark(
        history,
        BenchRecord::new("db_scan_time_range", "rows", scanned_rows, scan_elapsed)
            .with_context("iterations", scan_iterations)
            .with_context("table_rows", total_rows),
    )?;

    let predicate = RangePredicate::new("price", 100.0, 110.0);
    let predicate_start = Instant::now();
    let mut predicate_rows = 0usize;
    for _ in 0..scan_iterations {
        let result = table.scan_where((0, total_rows as i64), &predicate)?;
        predicate_rows = predicate_rows.saturating_add(result.row_count());
    }
    let predicate_elapsed = predicate_start.elapsed();
    info!(
        event = "benchmark_complete",
        benchmark = "db_scan_predicate",
        path = %path.display(),
        rows = predicate_rows,
        iterations = scan_iterations,
        elapsed_ms = predicate_elapsed.as_millis(),
        rows_per_sec = rate(predicate_rows, predicate_elapsed),
        "completed bypass-db predicate scan benchmark"
    );
    println!(
        "benchmark=db_scan_predicate rows={} iterations={} elapsed_ms={} rows_per_sec={:.2}",
        predicate_rows,
        scan_iterations,
        predicate_elapsed.as_millis(),
        rate(predicate_rows, predicate_elapsed)
    );
    record_benchmark(
        history,
        BenchRecord::new(
            "db_scan_predicate",
            "rows",
            predicate_rows,
            predicate_elapsed,
        )
        .with_context("iterations", scan_iterations)
        .with_context("table_rows", total_rows),
    )?;

    if compact {
        let segment_ids = table
            .manifest()
            .sealed_segments
            .iter()
            .map(|segment| segment.id)
            .collect::<Vec<_>>();
        let compact_start = Instant::now();
        table.compact(&segment_ids)?;
        let compact_elapsed = compact_start.elapsed();
        info!(
            event = "benchmark_complete",
            benchmark = "db_compact",
            path = %path.display(),
            segments = segment_ids.len(),
            elapsed_ms = compact_elapsed.as_millis(),
            "completed bypass-db compaction benchmark"
        );
        println!(
            "benchmark=db_compact segments={} elapsed_ms={}",
            segment_ids.len(),
            compact_elapsed.as_millis()
        );
        record_benchmark(
            history,
            BenchRecord::new("db_compact", "segments", segment_ids.len(), compact_elapsed),
        )?;
    }
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BenchRecord {
    benchmark: String,
    unit: String,
    units: usize,
    bytes: Option<usize>,
    elapsed_ms: u64,
    rate_per_sec: f64,
    mib_per_sec: Option<f64>,
    context: Vec<BenchContext>,
    timestamp_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BenchContext {
    key: String,
    value: usize,
}

impl BenchRecord {
    fn new(
        benchmark: impl Into<String>,
        unit: impl Into<String>,
        units: usize,
        elapsed: Duration,
    ) -> Self {
        Self {
            benchmark: benchmark.into(),
            unit: unit.into(),
            units,
            bytes: None,
            elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            rate_per_sec: rate(units, elapsed),
            mib_per_sec: None,
            context: Vec::new(),
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        }
    }

    fn with_bytes(mut self, bytes: usize) -> Self {
        self.bytes = Some(bytes);
        self.mib_per_sec = Some(mib_per_sec(
            bytes,
            Duration::from_millis(self.elapsed_ms.max(1)),
        ));
        self
    }

    fn with_context(mut self, key: impl Into<String>, value: usize) -> Self {
        self.context.push(BenchContext {
            key: key.into(),
            value,
        });
        self
    }
}

fn record_benchmark(history: Option<&Path>, record: BenchRecord) -> Result<(), CliError> {
    let Some(path) = history else {
        return Ok(());
    };
    let previous = latest_history_record(path, &record.benchmark)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(CliError::Io)?;
    serde_json::to_writer(&mut file, &record).map_err(CliError::Json)?;
    file.write_all(b"\n").map_err(CliError::Io)?;
    if let Some(previous) = previous {
        let delta = percent_delta(previous.rate_per_sec, record.rate_per_sec);
        println!(
            "benchmark_history benchmark={} previous_rate={:.2} current_rate={:.2} delta_percent={:.2}",
            record.benchmark, previous.rate_per_sec, record.rate_per_sec, delta
        );
    } else {
        println!(
            "benchmark_history benchmark={} previous_rate=none",
            record.benchmark
        );
    }
    Ok(())
}

fn latest_history_record(path: &Path, benchmark: &str) -> Result<Option<BenchRecord>, CliError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(CliError::Io)?;
    let mut latest = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record = serde_json::from_str::<BenchRecord>(line).map_err(CliError::Json)?;
        if record.benchmark == benchmark {
            latest = Some(record);
        }
    }
    Ok(latest)
}

fn percent_delta(previous: f64, current: f64) -> f64 {
    if previous.abs() <= f64::EPSILON {
        0.0
    } else {
        ((current - previous) / previous) * 100.0
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckState {
    Ok,
    Warn,
    Fail,
}

impl CheckState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostCheck {
    name: &'static str,
    state: CheckState,
    detail: String,
}

fn kernel_check() -> HostCheck {
    let detail = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| format!("linux {}", text.trim()))
        .unwrap_or_else(|_| {
            format!(
                "os={} arch={}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        });
    HostCheck {
        name: "kernel",
        state: CheckState::Ok,
        detail,
    }
}

fn hugepage_check(required: bool) -> HostCheck {
    match fs::read_to_string("/proc/meminfo") {
        Ok(meminfo) => {
            let total = meminfo_number(&meminfo, "HugePages_Total:").unwrap_or(0);
            let free = meminfo_number(&meminfo, "HugePages_Free:").unwrap_or(0);
            let size = meminfo_hugepage_size(&meminfo).unwrap_or_else(|| "unknown".to_string());
            if total > 0 {
                HostCheck {
                    name: "hugepages",
                    state: CheckState::Ok,
                    detail: format!("total={total} free={free} size={size}"),
                }
            } else if required {
                HostCheck {
                    name: "hugepages",
                    state: CheckState::Fail,
                    detail: "HugePages_Total=0; configure hugepages before hardware validation"
                        .to_string(),
                }
            } else {
                HostCheck {
                    name: "hugepages",
                    state: CheckState::Warn,
                    detail:
                        "HugePages_Total=0; Rust checks can run, but hardware validation needs hugepages"
                            .to_string(),
                }
            }
        }
        Err(err) => HostCheck {
            name: "hugepages",
            state: if required {
                CheckState::Fail
            } else {
                CheckState::Warn
            },
            detail: format!("/proc/meminfo is not readable: {err}"),
        },
    }
}

fn meminfo_number(meminfo: &str, key: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

fn meminfo_hugepage_size(meminfo: &str) -> Option<String> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("Hugepagesize:"))
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|value| !value.is_empty())
}

fn path_check(
    name: &'static str,
    path: &Path,
    missing_state: CheckState,
    present_detail: &'static str,
    missing_detail: &'static str,
) -> HostCheck {
    if path.exists() {
        HostCheck {
            name,
            state: CheckState::Ok,
            detail: present_detail.to_string(),
        }
    } else {
        HostCheck {
            name,
            state: missing_state,
            detail: missing_detail.to_string(),
        }
    }
}

fn pci_check(name: &'static str, bdf: Option<&str>) -> HostCheck {
    let Some(bdf) = bdf.filter(|value| !value.trim().is_empty()) else {
        return HostCheck {
            name,
            state: CheckState::Warn,
            detail: "no PCI BDF provided".to_string(),
        };
    };
    let path = Path::new("/sys/bus/pci/devices").join(bdf);
    if !path.exists() {
        return HostCheck {
            name,
            state: CheckState::Fail,
            detail: format!("PCI device {bdf} was not found under /sys/bus/pci/devices"),
        };
    }
    let driver = fs::read_link(path.join("driver")).ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    match driver {
        Some(driver) => HostCheck {
            name,
            state: CheckState::Ok,
            detail: format!("device={bdf} driver={driver}"),
        },
        None => HostCheck {
            name,
            state: CheckState::Warn,
            detail: format!("device={bdf} exists but has no bound driver"),
        },
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
    /// Inspect native runtime and host readiness.
    Doctor {
        /// Doctor command.
        #[command(subcommand)]
        command: DoctorCommand,
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
        /// JSON-lines history file for recording and comparing benchmark runs.
        #[arg(long)]
        history: Option<PathBuf>,
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
        /// Number of scan and predicate-scan iterations after append/flush.
        #[arg(long, default_value_t = 10)]
        scan_iterations: usize,
        /// Benchmark segment compaction after scan benchmarks.
        #[arg(long)]
        compact: bool,
        /// JSON-lines history file for recording and comparing benchmark runs.
        #[arg(long)]
        history: Option<PathBuf>,
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

#[derive(Debug, Subcommand)]
enum DoctorCommand {
    /// Inspect native SPDK/DPDK status and hardware host prerequisites.
    Native {
        /// Optional SPDK/NVMe PCI BDF, for example `0000:01:00.0`.
        #[arg(long)]
        spdk_pci: Option<String>,
        /// Optional DPDK/NIC PCI BDF, for example `0000:02:00.0`.
        #[arg(long)]
        dpdk_pci: Option<String>,
        /// Fail when hugepages are not configured.
        #[arg(long)]
        require_hugepages: bool,
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
    Json(serde_json::Error),
    DoctorFailed { failures: usize },
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Unsupported(_) => 3,
            Self::Io(_)
            | Self::Config(_)
            | Self::Schema(_)
            | Self::Batch(_)
            | Self::Table(_)
            | Self::Json(_) => 1,
            Self::DoctorFailed { .. } => 2,
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
            Self::Json(err) => write!(f, "{err}"),
            Self::DoctorFailed { failures } => {
                write!(f, "native doctor found {failures} required failure(s)")
            }
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

    use super::{meminfo_hugepage_size, meminfo_number, parse_duration, CheckState};

    #[test]
    fn parse_duration_supports_common_suffixes() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("10s"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert!(parse_duration("bad").is_err());
    }

    #[test]
    fn meminfo_helpers_parse_hugepage_fields() {
        let meminfo = "\
HugePages_Total:    1024
HugePages_Free:      900
Hugepagesize:       2048 kB
";
        assert_eq!(meminfo_number(meminfo, "HugePages_Total:"), Some(1024));
        assert_eq!(meminfo_number(meminfo, "HugePages_Free:"), Some(900));
        assert_eq!(meminfo_hugepage_size(meminfo).as_deref(), Some("2048 kB"));
    }

    #[test]
    fn check_state_has_stable_text() {
        assert_eq!(CheckState::Ok.as_str(), "ok");
        assert_eq!(CheckState::Warn.as_str(), "warn");
        assert_eq!(CheckState::Fail.as_str(), "fail");
    }
}
