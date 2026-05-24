//! Backend traits and device target identifiers.
//!
//! Backends hide the transport-specific mechanics of an I/O device behind a
//! common interface. The trait in this module is object-safe so higher-level
//! crates can store a backend behind `Arc<dyn IoBackend<Error = E>>` when they
//! need runtime dispatch.

use std::error::Error;
use std::future::Future;
use std::os::fd::RawFd;
use std::pin::Pin;

use crate::buf::PooledBuf;

/// Heap-allocated future returned by [`IoBackend`] methods.
///
/// The project specification sketches `impl Future` returns for maximum
/// monomorphized performance. This alias deliberately boxes futures so the
/// first trait can be object-safe. Hot-path backends may later add a separate
/// generic trait for static dispatch.
pub type BoxIoFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

/// Identifies the device or endpoint targeted by an I/O operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceTarget {
    /// Unix file descriptor used by the `io_uring` backend.
    Fd(RawFd),
    /// NVMe namespace identifier used by the SPDK backend.
    NvmeNs { nsid: u32 },
    /// Ethernet port identifier used by the DPDK backend.
    NetPort(u16),
}

/// Core asynchronous I/O operations shared by all backends.
///
/// Implementations must keep buffer lifetime and cancellation behavior explicit:
/// the returned future borrows the buffer for as long as the operation can
/// access it.
pub trait IoBackend: Send + Sync + 'static {
    /// Backend-specific error type.
    type Error: Error + Send + Sync + 'static;

    /// Read into `buf` from `target` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns the backend error when submission fails, completion reports a
    /// device error, or the target type is invalid for the backend.
    fn read<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a mut PooledBuf,
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error>;

    /// Write bytes from `buf` to `target` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns the backend error when submission fails, completion reports a
    /// device error, or the target type is invalid for the backend.
    fn write<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a PooledBuf,
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error>;

    /// Ensure previously submitted writes are durable when the backend supports
    /// persistence.
    ///
    /// # Errors
    ///
    /// Returns the backend error if the flush command cannot be submitted or
    /// completed successfully.
    fn flush<'a>(&'a self, target: DeviceTarget) -> BoxIoFuture<'a, (), Self::Error>;

    /// Drain available completions without blocking.
    fn poll_completions(&self) -> usize;
}
