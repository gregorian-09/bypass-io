#![doc = include_str!("../README.md")]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod buf;
pub mod config;
#[cfg(any(feature = "dpdk", feature = "spdk"))]
pub mod ffi;
pub mod reactor;
pub mod ring;

#[cfg(feature = "dpdk")]
pub use backend::dpdk::{
    DpdkBackend, DpdkConfig, DpdkError, DpdkPortConfig, EtherType, EthernetHeader, Ipv4Header,
    MulticastGroup, Packet, QueueId, UdpHeader,
};
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
pub use config::{
    BufPoolConfig, BypassConfig, ConfigError, DbColumnConfig, DbConfig, DbSchemaConfig,
    DpdkRuntimeConfig, ReactorConfig, SpdkRuntimeConfig, UringConfig,
};
pub use reactor::{PollReactor, ReactorHandle};
pub use ring::{MpscRing, SpscRing};
