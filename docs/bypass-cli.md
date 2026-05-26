# bypass-cli

`bypass-cli` is the local configuration and benchmark harness for the
`bypass-io` workspace.

## Configuration

Generate a default runtime configuration:

```bash
cargo run -p bypass-cli -- config default --output /tmp/bypass-io.toml
```

Validate a runtime configuration:

```bash
cargo run -p bypass-cli -- config validate --file /tmp/bypass-io.toml
```

The configuration model is backed by `serde` and `toml`, then validated by
`BypassConfig::validate` before use.

## Benchmarks

Run a local `io_uring` write benchmark:

```bash
cargo run --release -p bypass-cli -- bench uring \
  --file /tmp/bypass-uring.bin \
  --buf-size 4096 \
  --depth 1 \
  --duration 10s \
  --history /tmp/bypass-bench-history.jsonl
```

Run a local `bypass-db` append benchmark:

```bash
cargo run --release -p bypass-cli -- bench db \
  --path /tmp/bypass-db \
  --rows-per-batch 10000 \
  --batches 1000 \
  --scan-iterations 10 \
  --history /tmp/bypass-bench-history.jsonl
```

The database benchmark now reports append throughput, mmap-backed time-range
scan throughput, and predicate scan throughput. Add `--compact` to time segment
compaction after the scan measurements:

```bash
cargo run --release -p bypass-cli -- bench db \
  --path /tmp/bypass-db \
  --rows-per-batch 10000 \
  --batches 1000 \
  --scan-iterations 10 \
  --compact \
  --history /tmp/bypass-bench-history.jsonl
```

When `--history` is provided, the CLI appends one JSON object per benchmark
measurement to the given JSON-lines file. It also compares the current rate to
the latest previous record with the same benchmark name and prints a
`benchmark_history` line.

`bench spdk` and `bench dpdk` currently return explicit unsupported errors.
They are reserved for native runtime benchmark paths once the workspace can run
against bound NVMe and NIC hardware.

## Structured Events

Add `--trace-json` before the subcommand to emit structured tracing events:

```bash
cargo run -p bypass-cli -- --trace-json bench db \
  --path /tmp/bypass-db \
  --rows-per-batch 1000 \
  --batches 10
```

The CLI still prints a compact human-readable benchmark line on stdout.
Structured tracing events are emitted by `tracing-subscriber` for log and
metrics ingestion.
