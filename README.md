# bypass-io

`bypass-io` is a Rust systems workspace for low-latency I/O experiments:
kernel-adjacent file I/O, SPDK-style NVMe readiness, DPDK-style packet I/O
readiness, and an embedded columnar time-series layer.

The project is intentionally staged. Normal Rust builds remain lightweight and
do not require native SPDK/DPDK libraries, hugepages, or bound PCI devices. The
native paths are exposed as explicit validation surfaces until unsafe hardware
I/O is implemented and verified on dedicated machines.

## Workspace

- `bypass-io`: core buffers, rings, reactors, configuration, `io_uring`, SPDK,
  and DPDK backend surfaces.
- `bypass-db`: embedded columnar table storage with WAL, sealed segments,
  mmap-backed scans, compaction, and SIMD-assisted filtering.
- `bypass-cli`: configuration, benchmark, benchmark-history, and native
  readiness commands.

## Quick Start

Run the default validation set:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps --all-features
```

Inspect native readiness without mutating the host:

```bash
cargo run -p bypass-cli -- doctor native
```

Run a small database benchmark:

```bash
cargo run -p bypass-cli -- bench db \
  --path /tmp/bypass-db \
  --rows-per-batch 1000 \
  --batches 10 \
  --scan-iterations 2 \
  --compact
```

Compile public examples:

```bash
cargo check --workspace --examples --all-features
```

Run the table lifecycle example:

```bash
cargo run -p bypass-db --example table_lifecycle
```

## Native Boundary

The SPDK and DPDK Rust features compile validation surfaces by default. They do
not link native C libraries unless the native link-check environment variables
are set:

```bash
BYPASS_IO_NATIVE_SPDK=1 SPDK_LIB_DIR=/opt/spdk/build/lib \
  cargo test -p bypass-io --features spdk

BYPASS_IO_NATIVE_DPDK=1 \
  cargo test -p bypass-io --features dpdk
```

Even when native link checks pass, real SPDK/DPDK I/O remains disabled until
hardware I/O is explicitly enabled on a prepared host:

```bash
BYPASS_IO_ENABLE_SPDK_HARDWARE=1
BYPASS_IO_ENABLE_DPDK_HARDWARE=1
```

Only set these on dedicated machines with hugepages and safe VFIO-bound test
devices.

## Documentation

- `docs/validation-tiers.md`: local, CI, native, hardware, and performance
  validation tiers.
- `docs/native-runtime-adapters.md`: unsafe native adapter boundary and safety
  requirements.
- `docs/native-linking.md`: opt-in SPDK/DPDK native link-check commands.
- `docs/hardware-validation.md`: host readiness and self-hosted runner usage.
- `docs/release.md`: package validation before release.
- `docs/bypass-cli.md`: CLI commands.
- `docs/bypass-db-storage.md`: columnar storage and scan design.

## Examples

- `bypass-io/examples/uring_write.rs`: guarded `io_uring` write/read/fsync flow
  against a temporary file.
- `bypass-db/examples/table_lifecycle.rs`: schema creation, append, flush,
  mmap-backed scan, compaction, and cleanup.
