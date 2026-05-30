# Native Runtime Adapters

The native runtime adapters are the boundary between safe Rust backend APIs and
unsafe SPDK/DPDK C calls.

The current implementation includes hardware-gated native adapter call paths.
They compile only when the build script enables `bypass_io_native_spdk` or
`bypass_io_native_dpdk`. They remain disabled by default at runtime and return
`RuntimeUnavailable` until the operator explicitly enables hardware I/O with:

```bash
export BYPASS_IO_ENABLE_SPDK_HARDWARE=1
export BYPASS_IO_ENABLE_DPDK_HARDWARE=1
```

This keeps ordinary native link checks non-mutating while still providing the
real adapter boundary for dedicated hardware hosts.

## SPDK Adapter Boundary

The SPDK adapter scaffold lives inside:

```text
bypass-io/src/backend/spdk.rs
```

The raw FFI declarations live in:

```text
bypass-io/src/ffi/spdk_sys.rs
```

The adapter reserves the following native symbols for the eventual NVMe path:

- `spdk_nvme_probe`
- `spdk_nvme_ctrlr_alloc_io_qpair`
- `spdk_nvme_ns_cmd_read`
- `spdk_nvme_ns_cmd_write`
- `spdk_nvme_ns_cmd_flush`
- `spdk_nvme_qpair_process_completions`
- `spdk_zmalloc`
- `spdk_free`

The adapter now uses a small C shim for SPDK environment setup and completion
status helpers:

```text
bypass-io/native/bypass_spdk_shim.c
```

When `BYPASS_IO_ENABLE_SPDK_HARDWARE=1`, `SpdkBackend::probe_and_init()`:

- initializes the SPDK environment
- probes controllers
- discovers active namespaces
- allocates one I/O qpair
- enables SPDK DMA allocation for later `BufPool` allocations
- submits read, write, and flush commands through SPDK
- polls the qpair synchronously until completion or timeout

Before production use, the hardware validation phase must prove:

- Submitted buffers remain alive until SPDK completion callbacks run.
- DMA buffers are page-locked and acceptable to the selected SPDK environment.
- Completion callbacks cannot outlive their Rust completion state.
- Queue-pair polling is tied to one reactor thread or otherwise synchronized.
- NVMe completion status is translated into `SpdkError` without losing failure
  detail.

## DPDK Adapter Boundary

The DPDK adapter scaffold lives inside:

```text
bypass-io/src/backend/dpdk.rs
```

The raw FFI declarations live in:

```text
bypass-io/src/ffi/dpdk_sys.rs
```

The adapter reserves the following native APIs for the eventual Ethernet path:

- `rte_eal_init`
- `rte_pktmbuf_pool_create`
- `rte_eth_dev_configure`
- `rte_eth_rx_queue_setup`
- `rte_eth_tx_queue_setup`
- `rte_eth_dev_start`
- `rte_eth_rx_burst`
- `rte_eth_tx_burst`
- `rte_flow_create`

The adapter now uses a small C shim for DPDK inline/header APIs and default port
setup:

```text
bypass-io/native/bypass_dpdk_shim.c
```

When `BYPASS_IO_ENABLE_DPDK_HARDWARE=1`, `DpdkBackend::init()`:

- initializes DPDK EAL from `DpdkConfig::eal_args`
- creates an mbuf pool
- configures RX/TX queues with default DPDK queue configs
- starts the configured Ethernet port
- implements `rx_burst` and `tx_burst` through shimmed DPDK burst calls
- copies RX mbuf bytes into safe `Packet` values and frees received mbufs
- allocates TX mbufs, copies packet bytes into them, and frees unsent mbufs

DPDK documents RX/TX burst operations as inline header APIs in common releases.
That means the future runtime may need bindgen output or a small C shim instead
of a direct `extern "C"` declaration for every packet fast-path operation.

Before production use, the hardware validation phase must prove:

- DPDK EAL is initialized once per process.
- Mbuf ownership is explicit from RX through parsing, release, or TX.
- RX/TX queue access follows DPDK's queue/threading rules.
- Packet data is not exposed through Rust references after the mbuf is freed.
- Flow rules are destroyed or intentionally owned for the backend lifetime.

## Runtime Status

`SpdkBackend::native_status()` and `DpdkBackend::native_status()` distinguish
three states:

- Default validation build: `linked = false`.
- Native link-check build: `linked = true`, adapter code compiled, hardware I/O
  disabled unless the runtime opt-in environment variable is set.
- Hardware runtime build: `linked = true`, runtime opt-in is set, native calls
  execute on a dedicated hardware host.

The third state requires a prepared hardware machine. Hosted CI does not execute
real native I/O.

## File-Backed Runtime Tests

The unit tests include file-backed SPDK and DPDK runtime implementations. These
are not public backends and do not link native libraries. They exist to prove
the Rust-side backend pipeline before unsafe native calls are enabled:

- `PooledBuf` checkout and byte access
- backend target validation
- SPDK buffer-segment construction and DMA eligibility checks
- SPDK byte offset to LBA conversion
- runtime read/write delegation through the same `IoBackend` futures used by
  callers
- DPDK runtime read/write delegation through the network-port target path

The SPDK file-backed test uses a temporary file as a namespace image and issues
`pread`/`pwrite` at the byte offset derived from the validated LBA range. On
hosts where Linux does not expose a page-locked, physical-address-visible buffer
to the process, the test accepts the expected DMA eligibility rejection instead
of pretending the buffer could be submitted to SPDK.

The DPDK file-backed test uses a temporary file as a deterministic packet data
source/sink. DPDK has no byte-offset target in the shared `IoBackend` contract,
so the test validates buffer movement through the port and queue runtime seam
rather than storage-style LBA translation.
