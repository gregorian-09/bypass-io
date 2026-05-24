# bypass-io

`bypass-io` provides the core building blocks for a kernel-bypass I/O stack:
DMA-oriented buffers, bounded rings, backend traits, and busy-poll reactors.

This crate is currently in Phase 1. The implemented surface is intentionally
small and focuses on core ownership and reactor primitives before SPDK and DPDK
FFI layers are introduced.
