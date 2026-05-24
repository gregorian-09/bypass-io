#![doc = include_str!("../README.md")]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod buf;
#[cfg(feature = "spdk")]
pub mod ffi;
pub mod reactor;
pub mod ring;

#[cfg(feature = "spdk")]
pub use backend::spdk::{
    IoQueuePair, NvmeController, NvmeLbaRange, NvmeNamespace, SpdkBackend, SpdkError,
};
#[cfg(feature = "uring")]
pub use backend::uring::UringBackend;
pub use backend::{BoxIoFuture, DeviceTarget, IoBackend};
pub use buf::{
    BufPool, HugeBuf, HugeBufBacking, HugePageSize, IoVec, IoVecMut, PooledBuf, RawIoVec,
};
pub use reactor::{PollReactor, ReactorHandle};
pub use ring::{MpscRing, SpscRing};
