# bypass-io

`bypass-io` provides the core building blocks for a kernel-bypass I/O stack:
DMA-oriented buffers, bounded rings, backend traits, and busy-poll reactors.

The crate currently includes:

- hugepage-oriented buffers and reusable buffer pools
- SPSC and MPSC bounded rings
- busy-poll reactors and CPU-affinity helpers
- an object-safe `IoBackend` trait
- a default `io_uring` backend for local file I/O
- configuration loading and validation
- Rust-side SPDK and DPDK validation backends
- opt-in native SPDK/DPDK link checks
- native runtime adapter scaffolds with hardware I/O disabled

Normal builds do not require SPDK, DPDK, hugepages, or bound devices. Native
SPDK/DPDK paths are intentionally staged: link checks and adapter scaffolds are
available, but real hardware I/O remains disabled until the unsafe call paths
are completed and validated on dedicated hardware.
