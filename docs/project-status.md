# Project Status

This document summarizes what the public workspace currently implements and
where the boundaries still are.

## Implemented

### Core `bypass-io`

- Object-safe `IoBackend` trait for backend dispatch.
- `DeviceTarget` for file descriptors, NVMe namespaces, and network ports.
- Hugepage-oriented buffer ownership through `HugeBuf` and `PooledBuf`.
- Lock-free buffer-pool free list through `crossbeam-queue`.
- SPSC and MPSC bounded rings.
- Busy-poll reactor and Linux CPU-affinity helper.
- Default `io_uring` backend for file read, write, vectored read/write, and
  fsync.
- Runtime configuration backed by `serde` and `toml`.
- SPDK and DPDK Rust validation backends.
- Native SPDK/DPDK link-check build script path.
- Native SPDK/DPDK adapter scaffolds with I/O disabled.

### `bypass-db`

- Schema and row-batch validation.
- Checksummed write-ahead log.
- Active table appends and recovery.
- Immutable sealed segment files and manifests.
- Mmap-backed sealed column access.
- Chunked `ScanResult` values that can retain mapped sealed columns.
- SIMD-assisted filtering for supported x86_64 scan paths with scalar
  fallbacks.
- Segment compaction.

### `bypass-cli`

- Configuration default generation and validation.
- `io_uring` write benchmark.
- `bypass-db` append, scan, predicate-scan, and compaction benchmark.
- JSON-lines benchmark history and previous-run comparison.
- Native readiness doctor for SPDK/DPDK status and host prerequisites.
- Structured tracing output through `--trace-json`.

### Validation and Operations

- Lightweight Rust CI for formatting, tests, clippy, docs, DB smoke benchmark,
  native readiness smoke, and `.internal` tracking guard.
- Manual native SPDK and DPDK build workflows.
- Optional Docker recipes for Rust, SPDK, and DPDK validation environments.
- Manual self-hosted hardware-validation workflow.
- Read-only hardware host validation script.

## Explicit Boundaries

Real native SPDK/DPDK device I/O is not enabled yet.

The current native states are:

- Default build: SPDK/DPDK native status reports `linked = false`.
- Native link-check build: status can report `linked = true`, but adapter I/O
  remains disabled.
- Future hardware runtime: native adapters must call audited C APIs and pass
  hardware tests before benchmarks can use SPDK NVMe or DPDK RX/TX paths.

`bench spdk` and `bench dpdk` remain intentionally unsupported until that
native-runtime phase is complete.

## Next Work

The highest-value remaining phases are:

1. Complete the SPDK native runtime adapter for a minimal read/write/flush path.
2. Complete the DPDK native runtime adapter for RX/TX burst ownership.
3. Add hardware-runner tests that exercise bound NVMe and NIC devices.
4. Add release packaging metadata and examples for downstream users.
5. Audit public rustdoc for examples and failure-mode coverage.
