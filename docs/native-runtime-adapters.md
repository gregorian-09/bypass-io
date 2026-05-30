# Native Runtime Adapters

The native runtime adapters are the boundary between safe Rust backend APIs and
unsafe SPDK/DPDK C calls.

The current implementation intentionally adds adapter scaffolds, not live native
I/O. The scaffolds compile only when the build script enables
`bypass_io_native_spdk` or `bypass_io_native_dpdk`, and every operation still
returns `RuntimeUnavailable`. That keeps the project honest: native link flags
can be validated before the unsafe call paths are trusted.

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

Before those calls are enabled, the implementation must prove:

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

DPDK documents RX/TX burst operations as inline header APIs in common releases.
That means the future runtime may need bindgen output or a small C shim instead
of a direct `extern "C"` declaration for every packet fast-path operation.

Before those calls are enabled, the implementation must prove:

- DPDK EAL is initialized once per process.
- Mbuf ownership is explicit from RX through parsing, release, or TX.
- RX/TX queue access follows DPDK's queue/threading rules.
- Packet data is not exposed through Rust references after the mbuf is freed.
- Flow rules are destroyed or intentionally owned for the backend lifetime.

## Runtime Status

`SpdkBackend::native_status()` and `DpdkBackend::native_status()` now distinguish
three states:

- Default validation build: `linked = false`.
- Native link-check build: `linked = true`, adapter scaffold compiled, I/O
  disabled.
- Future native runtime build: `linked = true`, adapter calls audited native
  functions and passes hardware tests.

Only the first two states exist today.

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
