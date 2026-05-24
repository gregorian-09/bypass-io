#![doc = include_str!("../README.md")]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod buf;
pub mod reactor;
pub mod ring;

pub use backend::{BoxIoFuture, DeviceTarget, IoBackend};
pub use buf::{BufPool, HugeBuf, HugePageSize, PooledBuf};
pub use reactor::{PollReactor, ReactorHandle};
pub use ring::SpscRing;
