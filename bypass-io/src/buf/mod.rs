//! DMA-oriented buffer primitives.
//!
//! [`HugeBuf`] owns a page-aligned memory mapping intended for registration
//! with a low-latency I/O backend. [`BufPool`] keeps a fixed set of buffers
//! available so hot paths can reuse memory rather than allocate per operation.

mod hugepage;
mod iovec;
mod pool;

pub use hugepage::{HugeBuf, HugeBufBacking, HugePageSize};
pub use iovec::{IoVec, IoVecMut, RawIoVec};
pub use pool::{BufPool, PooledBuf};
