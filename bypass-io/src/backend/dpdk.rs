//! DPDK Ethernet backend.
//!
//! Phase 3 establishes the safe Rust side of the DPDK backend: EAL/port
//! configuration descriptors, RX/TX burst APIs, zero-copy-style packet parsing,
//! multicast flow-rule validation, and the [`IoBackend`] implementation. Native
//! DPDK C calls are isolated behind a private runtime trait so the crate remains
//! buildable without DPDK installed.

use std::error::Error;
use std::fmt;
use std::net::Ipv4Addr;
use std::sync::Arc;

use crate::backend::{BoxIoFuture, DeviceTarget, IoBackend};
use crate::buf::PooledBuf;

/// Queue identifier local to a DPDK port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueId(u16);

impl QueueId {
    /// Create a queue identifier.
    #[must_use]
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    /// Return the raw queue id.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// DPDK port configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpdkPortConfig {
    port_id: u16,
    rx_queues: u16,
    tx_queues: u16,
    rx_desc: u16,
    tx_desc: u16,
    promiscuous: bool,
}

impl DpdkPortConfig {
    /// Create a port configuration.
    ///
    /// # Errors
    ///
    /// Returns [`DpdkError::InvalidConfig`] when any queue or descriptor count
    /// is zero.
    pub fn new(
        port_id: u16,
        rx_queues: u16,
        tx_queues: u16,
        rx_desc: u16,
        tx_desc: u16,
        promiscuous: bool,
    ) -> Result<Self, DpdkError> {
        if rx_queues == 0 {
            return Err(DpdkError::InvalidConfig("rx_queues must be non-zero"));
        }
        if tx_queues == 0 {
            return Err(DpdkError::InvalidConfig("tx_queues must be non-zero"));
        }
        if rx_desc == 0 {
            return Err(DpdkError::InvalidConfig("rx_desc must be non-zero"));
        }
        if tx_desc == 0 {
            return Err(DpdkError::InvalidConfig("tx_desc must be non-zero"));
        }
        Ok(Self {
            port_id,
            rx_queues,
            tx_queues,
            rx_desc,
            tx_desc,
            promiscuous,
        })
    }

    /// Port id.
    #[must_use]
    pub fn port_id(&self) -> u16 {
        self.port_id
    }

    /// RX queue count.
    #[must_use]
    pub fn rx_queues(&self) -> u16 {
        self.rx_queues
    }

    /// TX queue count.
    #[must_use]
    pub fn tx_queues(&self) -> u16 {
        self.tx_queues
    }

    /// RX descriptor count per queue.
    #[must_use]
    pub fn rx_desc(&self) -> u16 {
        self.rx_desc
    }

    /// TX descriptor count per queue.
    #[must_use]
    pub fn tx_desc(&self) -> u16 {
        self.tx_desc
    }

    /// Whether the port should be placed in promiscuous mode.
    #[must_use]
    pub fn promiscuous(&self) -> bool {
        self.promiscuous
    }
}

/// DPDK backend configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpdkConfig {
    eal_args: Vec<String>,
    port: DpdkPortConfig,
    mbufs: u32,
    mbuf_cache: u32,
    data_room_size: u16,
    socket_id: i32,
}

impl DpdkConfig {
    /// Create a DPDK configuration.
    ///
    /// # Errors
    ///
    /// Returns [`DpdkError::InvalidConfig`] when EAL arguments, mempool sizing,
    /// or packet data-room settings are invalid.
    pub fn new(
        eal_args: Vec<String>,
        port: DpdkPortConfig,
        mbufs: u32,
        mbuf_cache: u32,
        data_room_size: u16,
        socket_id: i32,
    ) -> Result<Self, DpdkError> {
        if eal_args.is_empty() {
            return Err(DpdkError::InvalidConfig(
                "at least one EAL argument is required",
            ));
        }
        if eal_args.iter().any(|arg| arg.is_empty()) {
            return Err(DpdkError::InvalidConfig("EAL arguments must be non-empty"));
        }
        if mbufs == 0 {
            return Err(DpdkError::InvalidConfig("mbufs must be non-zero"));
        }
        if data_room_size == 0 {
            return Err(DpdkError::InvalidConfig("data_room_size must be non-zero"));
        }
        Ok(Self {
            eal_args,
            port,
            mbufs,
            mbuf_cache,
            data_room_size,
            socket_id,
        })
    }

    /// DPDK EAL arguments.
    #[must_use]
    pub fn eal_args(&self) -> &[String] {
        &self.eal_args
    }

    /// Port configuration.
    #[must_use]
    pub fn port(&self) -> &DpdkPortConfig {
        &self.port
    }

    /// Configured DPDK port id.
    #[must_use]
    pub fn port_id(&self) -> u16 {
        self.port.port_id()
    }

    /// Number of mbufs requested for the mempool.
    #[must_use]
    pub fn mbufs(&self) -> u32 {
        self.mbufs
    }

    /// Per-core mbuf cache size.
    #[must_use]
    pub fn mbuf_cache(&self) -> u32 {
        self.mbuf_cache
    }

    /// Packet data-room size.
    #[must_use]
    pub fn data_room_size(&self) -> u16 {
        self.data_room_size
    }

    /// NUMA socket id.
    #[must_use]
    pub fn socket_id(&self) -> i32 {
        self.socket_id
    }
}

/// DPDK poll-mode backend.
#[derive(Clone)]
pub struct DpdkBackend {
    config: DpdkConfig,
    runtime: Arc<dyn DpdkRuntime>,
}

impl fmt::Debug for DpdkBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DpdkBackend")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl DpdkBackend {
    /// Return the native DPDK runtime integration status for this build.
    #[must_use]
    #[cfg(bypass_io_native_dpdk)]
    pub const fn native_status() -> DpdkNativeStatus {
        DpdkNativeStatus {
            linked: true,
            detail:
                "native DPDK link flags are active; safe runtime adapter is still validation-only",
        }
    }

    /// Return the native DPDK runtime integration status for this build.
    #[must_use]
    #[cfg(not(bypass_io_native_dpdk))]
    pub const fn native_status() -> DpdkNativeStatus {
        DpdkNativeStatus {
            linked: false,
            detail: "native DPDK symbols are not linked; Rust validation runtime is active",
        }
    }

    /// Initialize DPDK and start the configured Ethernet port.
    ///
    /// # Errors
    ///
    /// Returns [`DpdkError::RuntimeUnavailable`] until the native DPDK runtime
    /// adapter is implemented.
    pub fn init(config: DpdkConfig) -> Result<Self, DpdkError> {
        let backend = Self::unavailable(config);
        backend.runtime.init(backend.config())?;
        Ok(backend)
    }

    /// Build a metadata-only backend that validates DPDK requests but returns
    /// [`DpdkError::RuntimeUnavailable`] for native operations.
    #[must_use]
    pub fn unavailable(config: DpdkConfig) -> Self {
        Self {
            config,
            runtime: Arc::new(UnavailableDpdkRuntime),
        }
    }

    /// Return the backend configuration.
    #[must_use]
    pub fn config(&self) -> &DpdkConfig {
        &self.config
    }

    /// Receive up to `max_packets` packets from an RX queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue id is invalid or the native runtime is
    /// unavailable.
    pub fn rx_burst(&self, queue: QueueId, max_packets: u16) -> Result<Vec<Packet>, DpdkError> {
        self.check_rx_queue(queue)?;
        if max_packets == 0 {
            return Ok(Vec::new());
        }
        self.runtime
            .rx_burst(self.config.port_id(), queue, max_packets)
    }

    /// Transmit a packet batch on a TX queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue id is invalid, the packet batch is empty,
    /// or the native runtime is unavailable.
    pub fn tx_burst(&self, queue: QueueId, packets: &[Packet]) -> Result<u16, DpdkError> {
        self.check_tx_queue(queue)?;
        if packets.is_empty() {
            return Err(DpdkError::EmptyPacketBatch);
        }
        self.runtime.tx_burst(self.config.port_id(), queue, packets)
    }

    /// Join a UDP multicast group by installing a DPDK flow rule.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue is invalid, the group is not multicast,
    /// the port is zero, or the native runtime is unavailable.
    pub fn join_multicast(&self, group: MulticastGroup, queue: QueueId) -> Result<(), DpdkError> {
        self.check_rx_queue(queue)?;
        group.validate()?;
        self.runtime
            .join_multicast(self.config.port_id(), group, queue)
    }

    fn check_rx_queue(&self, queue: QueueId) -> Result<(), DpdkError> {
        if queue.get() >= self.config.port.rx_queues() {
            Err(DpdkError::InvalidQueue {
                queue: queue.get(),
                configured: self.config.port.rx_queues(),
                kind: QueueKind::Rx,
            })
        } else {
            Ok(())
        }
    }

    fn check_tx_queue(&self, queue: QueueId) -> Result<(), DpdkError> {
        if queue.get() >= self.config.port.tx_queues() {
            Err(DpdkError::InvalidQueue {
                queue: queue.get(),
                configured: self.config.port.tx_queues(),
                kind: QueueKind::Tx,
            })
        } else {
            Ok(())
        }
    }

    fn ensure_target(&self, target: DeviceTarget) -> Result<(), DpdkError> {
        let DeviceTarget::NetPort(port) = target else {
            return Err(DpdkError::InvalidTarget {
                expected: "DeviceTarget::NetPort",
            });
        };
        if port != self.config.port_id() {
            return Err(DpdkError::PortMismatch {
                requested: port,
                configured: self.config.port_id(),
            });
        }
        Ok(())
    }

    #[cfg(test)]
    fn with_runtime(config: DpdkConfig, runtime: Arc<dyn DpdkRuntime>) -> Self {
        Self { config, runtime }
    }
}

/// Native DPDK runtime status for the current build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DpdkNativeStatus {
    /// Whether this build links a native DPDK runtime adapter.
    pub linked: bool,
    /// Human-readable status detail.
    pub detail: &'static str,
}

impl IoBackend for DpdkBackend {
    type Error = DpdkError;

    fn read<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a mut PooledBuf,
        _offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            self.ensure_target(target)?;
            self.runtime
                .read_packet(self.config.port_id(), QueueId::new(0), buf)
        })
    }

    fn write<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a PooledBuf,
        _offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            self.ensure_target(target)?;
            self.runtime
                .write_packet(self.config.port_id(), QueueId::new(0), buf)
        })
    }

    fn readv<'a>(
        &'a self,
        target: DeviceTarget,
        bufs: &'a mut [PooledBuf],
        _offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            self.ensure_target(target)?;
            if bufs.is_empty() {
                return Ok(0);
            }
            self.runtime
                .read_packets(self.config.port_id(), QueueId::new(0), bufs)
        })
    }

    fn writev<'a>(
        &'a self,
        target: DeviceTarget,
        bufs: &'a [PooledBuf],
        _offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            self.ensure_target(target)?;
            if bufs.is_empty() {
                return Ok(0);
            }
            self.runtime
                .write_packets(self.config.port_id(), QueueId::new(0), bufs)
        })
    }

    fn flush<'a>(&'a self, target: DeviceTarget) -> BoxIoFuture<'a, (), Self::Error> {
        Box::pin(async move {
            self.ensure_target(target)?;
            Ok(())
        })
    }

    fn poll_completions(&self) -> usize {
        self.runtime.poll_port(self.config.port_id())
    }
}

/// UDP multicast group descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MulticastGroup {
    group_ip: Ipv4Addr,
    port: u16,
}

impl MulticastGroup {
    /// Create a multicast group descriptor.
    #[must_use]
    pub const fn new(group_ip: Ipv4Addr, port: u16) -> Self {
        Self { group_ip, port }
    }

    /// Multicast IPv4 address.
    #[must_use]
    pub const fn group_ip(self) -> Ipv4Addr {
        self.group_ip
    }

    /// UDP destination port.
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port
    }

    fn validate(self) -> Result<(), DpdkError> {
        if !self.group_ip.is_multicast() {
            return Err(DpdkError::InvalidMulticastGroup {
                group_ip: self.group_ip,
                port: self.port,
            });
        }
        if self.port == 0 {
            return Err(DpdkError::InvalidMulticastGroup {
                group_ip: self.group_ip,
                port: self.port,
            });
        }
        Ok(())
    }
}

/// Parsed packet view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    data: Arc<[u8]>,
}

impl Packet {
    /// Create a packet view from owned bytes.
    ///
    /// This constructor is used by tests and by non-native runtimes. A native
    /// DPDK runtime can later replace the private storage with mbuf ownership
    /// without changing the parsing API.
    #[must_use]
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            data: Arc::from(bytes.into()),
        }
    }

    /// Raw packet bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Ethernet header.
    #[must_use]
    pub fn ethernet(&self) -> Option<EthernetHeader> {
        EthernetHeader::parse(self.bytes())
    }

    /// IPv4 header when the Ethernet type is IPv4.
    #[must_use]
    pub fn ipv4(&self) -> Option<Ipv4Header> {
        if self.ethernet()?.ether_type != EtherType::Ipv4 {
            return None;
        }
        Ipv4Header::parse(self.bytes().get(ETHERNET_HEADER_LEN..)?)
    }

    /// UDP header when the packet is Ethernet + IPv4 + UDP.
    #[must_use]
    pub fn udp(&self) -> Option<UdpHeader> {
        let ip = self.ipv4()?;
        if ip.protocol != IP_PROTO_UDP {
            return None;
        }
        let offset = ETHERNET_HEADER_LEN.checked_add(ip.header_len as usize)?;
        UdpHeader::parse(self.bytes().get(offset..)?)
    }

    /// UDP payload when the packet is Ethernet + IPv4 + UDP.
    #[must_use]
    pub fn udp_payload(&self) -> Option<&[u8]> {
        let ip = self.ipv4()?;
        if ip.protocol != IP_PROTO_UDP {
            return None;
        }
        let offset = ETHERNET_HEADER_LEN
            .checked_add(ip.header_len as usize)?
            .checked_add(UDP_HEADER_LEN)?;
        self.bytes().get(offset..)
    }
}

/// Ethernet EtherType.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EtherType {
    /// IPv4.
    Ipv4,
    /// IPv6.
    Ipv6,
    /// ARP.
    Arp,
    /// Any other EtherType.
    Other(u16),
}

impl From<u16> for EtherType {
    fn from(value: u16) -> Self {
        match value {
            0x0800 => Self::Ipv4,
            0x86dd => Self::Ipv6,
            0x0806 => Self::Arp,
            other => Self::Other(other),
        }
    }
}

/// Ethernet header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EthernetHeader {
    /// Destination MAC address.
    pub dst: [u8; 6],
    /// Source MAC address.
    pub src: [u8; 6],
    /// EtherType.
    pub ether_type: EtherType,
}

impl EthernetHeader {
    fn parse(data: &[u8]) -> Option<Self> {
        let header = data.get(..ETHERNET_HEADER_LEN)?;
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&header[0..6]);
        src.copy_from_slice(&header[6..12]);
        let ether_type = u16::from_be_bytes([header[12], header[13]]).into();
        Some(Self {
            dst,
            src,
            ether_type,
        })
    }
}

/// IPv4 header summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ipv4Header {
    /// Header length in bytes.
    pub header_len: u8,
    /// Protocol number.
    pub protocol: u8,
    /// Source address.
    pub src: Ipv4Addr,
    /// Destination address.
    pub dst: Ipv4Addr,
}

impl Ipv4Header {
    fn parse(data: &[u8]) -> Option<Self> {
        let first = *data.first()?;
        let version = first >> 4;
        if version != 4 {
            return None;
        }
        let ihl_words = first & 0x0f;
        let header_len = ihl_words.checked_mul(4)?;
        if header_len < IPV4_MIN_HEADER_LEN {
            return None;
        }
        let header = data.get(..header_len as usize)?;
        let protocol = header[9];
        let src = Ipv4Addr::new(header[12], header[13], header[14], header[15]);
        let dst = Ipv4Addr::new(header[16], header[17], header[18], header[19]);
        Some(Self {
            header_len,
            protocol,
            src,
            dst,
        })
    }
}

/// UDP header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UdpHeader {
    /// Source UDP port.
    pub src_port: u16,
    /// Destination UDP port.
    pub dst_port: u16,
    /// UDP datagram length.
    pub len: u16,
}

impl UdpHeader {
    fn parse(data: &[u8]) -> Option<Self> {
        let header = data.get(..UDP_HEADER_LEN)?;
        Some(Self {
            src_port: u16::from_be_bytes([header[0], header[1]]),
            dst_port: u16::from_be_bytes([header[2], header[3]]),
            len: u16::from_be_bytes([header[4], header[5]]),
        })
    }
}

/// DPDK backend errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DpdkError {
    /// Native DPDK runtime is not linked into this build.
    RuntimeUnavailable {
        /// Human-readable detail.
        detail: &'static str,
    },
    /// Invalid configuration.
    InvalidConfig(&'static str),
    /// A non-network target was passed to the DPDK backend.
    InvalidTarget {
        /// Expected target kind.
        expected: &'static str,
    },
    /// Requested port does not match the configured DPDK port.
    PortMismatch {
        /// Requested port.
        requested: u16,
        /// Configured port.
        configured: u16,
    },
    /// Queue id is outside the configured queue range.
    InvalidQueue {
        /// Requested queue.
        queue: u16,
        /// Configured queue count.
        configured: u16,
        /// Queue kind.
        kind: QueueKind,
    },
    /// Packet batch was empty where a non-empty batch is required.
    EmptyPacketBatch,
    /// Multicast group was invalid.
    InvalidMulticastGroup {
        /// Group IP.
        group_ip: Ipv4Addr,
        /// UDP destination port.
        port: u16,
    },
    /// DPDK operation returned an error code.
    OperationFailed {
        /// Operation name.
        operation: &'static str,
        /// Return code.
        rc: i32,
    },
}

impl fmt::Display for DpdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable { detail } => {
                write!(f, "DPDK runtime unavailable: {detail}")
            }
            Self::InvalidConfig(detail) => write!(f, "invalid DPDK config: {detail}"),
            Self::InvalidTarget { expected } => write!(f, "DPDK backend requires {expected}"),
            Self::PortMismatch {
                requested,
                configured,
            } => write!(
                f,
                "requested net port {requested} does not match configured port {configured}"
            ),
            Self::InvalidQueue {
                queue,
                configured,
                kind,
            } => write!(
                f,
                "{kind:?} queue {queue} is outside configured queue count {configured}"
            ),
            Self::EmptyPacketBatch => write!(f, "packet batch must not be empty"),
            Self::InvalidMulticastGroup { group_ip, port } => {
                write!(f, "invalid multicast group {group_ip}:{port}")
            }
            Self::OperationFailed { operation, rc } => {
                write!(f, "DPDK operation {operation} failed with rc={rc}")
            }
        }
    }
}

impl Error for DpdkError {}

/// Queue direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueKind {
    /// RX queue.
    Rx,
    /// TX queue.
    Tx,
}

trait DpdkRuntime: Send + Sync + 'static {
    fn init(&self, config: &DpdkConfig) -> Result<(), DpdkError>;

    fn rx_burst(
        &self,
        port_id: u16,
        queue: QueueId,
        max_packets: u16,
    ) -> Result<Vec<Packet>, DpdkError>;

    fn tx_burst(&self, port_id: u16, queue: QueueId, packets: &[Packet]) -> Result<u16, DpdkError>;

    fn join_multicast(
        &self,
        port_id: u16,
        group: MulticastGroup,
        queue: QueueId,
    ) -> Result<(), DpdkError>;

    fn read_packet(
        &self,
        port_id: u16,
        queue: QueueId,
        buf: &mut PooledBuf,
    ) -> Result<usize, DpdkError>;

    fn write_packet(
        &self,
        port_id: u16,
        queue: QueueId,
        buf: &PooledBuf,
    ) -> Result<usize, DpdkError>;

    fn read_packets(
        &self,
        port_id: u16,
        queue: QueueId,
        bufs: &mut [PooledBuf],
    ) -> Result<usize, DpdkError>;

    fn write_packets(
        &self,
        port_id: u16,
        queue: QueueId,
        bufs: &[PooledBuf],
    ) -> Result<usize, DpdkError>;

    fn poll_port(&self, port_id: u16) -> usize;
}

struct UnavailableDpdkRuntime;

impl DpdkRuntime for UnavailableDpdkRuntime {
    fn init(&self, _config: &DpdkConfig) -> Result<(), DpdkError> {
        Err(runtime_unavailable())
    }

    fn rx_burst(
        &self,
        _port_id: u16,
        _queue: QueueId,
        _max_packets: u16,
    ) -> Result<Vec<Packet>, DpdkError> {
        Err(runtime_unavailable())
    }

    fn tx_burst(
        &self,
        _port_id: u16,
        _queue: QueueId,
        _packets: &[Packet],
    ) -> Result<u16, DpdkError> {
        Err(runtime_unavailable())
    }

    fn join_multicast(
        &self,
        _port_id: u16,
        _group: MulticastGroup,
        _queue: QueueId,
    ) -> Result<(), DpdkError> {
        Err(runtime_unavailable())
    }

    fn read_packet(
        &self,
        _port_id: u16,
        _queue: QueueId,
        _buf: &mut PooledBuf,
    ) -> Result<usize, DpdkError> {
        Err(runtime_unavailable())
    }

    fn write_packet(
        &self,
        _port_id: u16,
        _queue: QueueId,
        _buf: &PooledBuf,
    ) -> Result<usize, DpdkError> {
        Err(runtime_unavailable())
    }

    fn read_packets(
        &self,
        _port_id: u16,
        _queue: QueueId,
        _bufs: &mut [PooledBuf],
    ) -> Result<usize, DpdkError> {
        Err(runtime_unavailable())
    }

    fn write_packets(
        &self,
        _port_id: u16,
        _queue: QueueId,
        _bufs: &[PooledBuf],
    ) -> Result<usize, DpdkError> {
        Err(runtime_unavailable())
    }

    fn poll_port(&self, _port_id: u16) -> usize {
        0
    }
}

fn runtime_unavailable() -> DpdkError {
    DpdkError::RuntimeUnavailable {
        detail: runtime_unavailable_detail(),
    }
}

#[cfg(bypass_io_native_dpdk)]
const fn runtime_unavailable_detail() -> &'static str {
    "native DPDK link flags are active, but the safe runtime adapter is not implemented"
}

#[cfg(not(bypass_io_native_dpdk))]
const fn runtime_unavailable_detail() -> &'static str {
    "native DPDK runtime is not linked"
}

const ETHERNET_HEADER_LEN: usize = 14;
const IPV4_MIN_HEADER_LEN: u8 = 20;
const UDP_HEADER_LEN: usize = 8;
const IP_PROTO_UDP: u8 = 17;

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{
        DpdkBackend, DpdkConfig, DpdkError, DpdkPortConfig, DpdkRuntime, EtherType, MulticastGroup,
        Packet, QueueId,
    };
    use crate::backend::{DeviceTarget, IoBackend};
    use crate::buf::{BufPool, HugePageSize, PooledBuf};

    #[derive(Default)]
    struct RecordingRuntime {
        polls: AtomicUsize,
    }

    impl DpdkRuntime for RecordingRuntime {
        fn init(&self, _config: &DpdkConfig) -> Result<(), DpdkError> {
            Ok(())
        }

        fn rx_burst(
            &self,
            _port_id: u16,
            _queue: QueueId,
            max_packets: u16,
        ) -> Result<Vec<Packet>, DpdkError> {
            Ok((0..max_packets)
                .map(|_| Packet::from_bytes(sample_udp_packet()))
                .collect())
        }

        fn tx_burst(
            &self,
            _port_id: u16,
            _queue: QueueId,
            packets: &[Packet],
        ) -> Result<u16, DpdkError> {
            Ok(packets.len() as u16)
        }

        fn join_multicast(
            &self,
            _port_id: u16,
            _group: MulticastGroup,
            _queue: QueueId,
        ) -> Result<(), DpdkError> {
            Ok(())
        }

        fn read_packet(
            &self,
            _port_id: u16,
            _queue: QueueId,
            buf: &mut PooledBuf,
        ) -> Result<usize, DpdkError> {
            Ok(buf.len())
        }

        fn write_packet(
            &self,
            _port_id: u16,
            _queue: QueueId,
            buf: &PooledBuf,
        ) -> Result<usize, DpdkError> {
            Ok(buf.len())
        }

        fn read_packets(
            &self,
            _port_id: u16,
            _queue: QueueId,
            bufs: &mut [PooledBuf],
        ) -> Result<usize, DpdkError> {
            Ok(bufs.iter().map(PooledBuf::len).sum())
        }

        fn write_packets(
            &self,
            _port_id: u16,
            _queue: QueueId,
            bufs: &[PooledBuf],
        ) -> Result<usize, DpdkError> {
            Ok(bufs.iter().map(PooledBuf::len).sum())
        }

        fn poll_port(&self, _port_id: u16) -> usize {
            self.polls.fetch_add(1, Ordering::Relaxed);
            4
        }
    }

    fn config() -> DpdkConfig {
        DpdkConfig::new(
            vec!["bypass-io".to_string(), "-l".to_string(), "0-1".to_string()],
            DpdkPortConfig::new(2, 2, 2, 512, 512, true).unwrap(),
            8192,
            256,
            2048,
            0,
        )
        .unwrap()
    }

    fn backend(runtime: Arc<dyn DpdkRuntime>) -> DpdkBackend {
        DpdkBackend::with_runtime(config(), runtime)
    }

    #[test]
    fn config_rejects_zero_queue_counts() {
        assert_eq!(
            DpdkPortConfig::new(0, 0, 1, 512, 512, false).unwrap_err(),
            DpdkError::InvalidConfig("rx_queues must be non-zero")
        );
        assert_eq!(
            DpdkPortConfig::new(0, 1, 0, 512, 512, false).unwrap_err(),
            DpdkError::InvalidConfig("tx_queues must be non-zero")
        );
    }

    #[test]
    fn packet_parses_ethernet_ipv4_udp_payload() {
        let packet = Packet::from_bytes(sample_udp_packet());
        assert_eq!(packet.ethernet().unwrap().ether_type, EtherType::Ipv4);
        assert_eq!(packet.ipv4().unwrap().protocol, 17);
        assert_eq!(packet.udp().unwrap().dst_port, 9000);
        assert_eq!(packet.udp_payload().unwrap(), b"DATA");
    }

    #[test]
    fn packet_rejects_truncated_headers() {
        assert!(Packet::from_bytes(vec![0u8; 13]).ethernet().is_none());
        assert!(Packet::from_bytes(vec![0u8; 20]).ipv4().is_none());
    }

    #[test]
    fn multicast_group_requires_multicast_ip_and_port() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(runtime);

        assert_eq!(
            backend
                .join_multicast(
                    MulticastGroup::new(Ipv4Addr::new(192, 168, 0, 1), 9000),
                    QueueId::new(0),
                )
                .unwrap_err(),
            DpdkError::InvalidMulticastGroup {
                group_ip: Ipv4Addr::new(192, 168, 0, 1),
                port: 9000
            }
        );
        assert_eq!(
            backend
                .join_multicast(
                    MulticastGroup::new(Ipv4Addr::new(239, 1, 2, 3), 0),
                    QueueId::new(0),
                )
                .unwrap_err(),
            DpdkError::InvalidMulticastGroup {
                group_ip: Ipv4Addr::new(239, 1, 2, 3),
                port: 0
            }
        );
    }

    #[test]
    fn burst_methods_validate_queue_bounds() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(runtime);

        assert_eq!(
            backend.rx_burst(QueueId::new(2), 1).unwrap_err(),
            DpdkError::InvalidQueue {
                queue: 2,
                configured: 2,
                kind: super::QueueKind::Rx
            }
        );
        assert_eq!(
            backend
                .tx_burst(QueueId::new(2), &[Packet::from_bytes(vec![1])])
                .unwrap_err(),
            DpdkError::InvalidQueue {
                queue: 2,
                configured: 2,
                kind: super::QueueKind::Tx
            }
        );
    }

    #[test]
    fn backend_rejects_wrong_io_target() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(runtime);
        let pool = BufPool::new(1, 64, HugePageSize::Mib2).unwrap();
        let buf = pool.acquire().unwrap();

        let err = block_on(backend.write(DeviceTarget::Fd(1), &buf, 0)).unwrap_err();
        assert_eq!(
            err,
            DpdkError::InvalidTarget {
                expected: "DeviceTarget::NetPort"
            }
        );
    }

    #[test]
    fn backend_rejects_mismatched_net_port() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(runtime);
        let pool = BufPool::new(1, 64, HugePageSize::Mib2).unwrap();
        let buf = pool.acquire().unwrap();

        let err = block_on(backend.write(DeviceTarget::NetPort(3), &buf, 0)).unwrap_err();
        assert_eq!(
            err,
            DpdkError::PortMismatch {
                requested: 3,
                configured: 2
            }
        );
    }

    #[test]
    fn poll_delegates_to_runtime() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(Arc::clone(&runtime) as Arc<dyn DpdkRuntime>);

        assert_eq!(backend.poll_completions(), 4);
        assert_eq!(runtime.polls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unavailable_backend_reports_runtime_unavailable() {
        #[cfg(not(bypass_io_native_dpdk))]
        assert_eq!(
            DpdkBackend::native_status(),
            super::DpdkNativeStatus {
                linked: false,
                detail: "native DPDK symbols are not linked; Rust validation runtime is active"
            }
        );
        #[cfg(bypass_io_native_dpdk)]
        assert_eq!(
            DpdkBackend::native_status(),
            super::DpdkNativeStatus {
                linked: true,
                detail: "native DPDK link flags are active; safe runtime adapter is still validation-only"
            }
        );
        #[cfg(not(bypass_io_native_dpdk))]
        assert_eq!(
            DpdkBackend::init(config()).unwrap_err(),
            DpdkError::RuntimeUnavailable {
                detail: "native DPDK runtime is not linked"
            }
        );
        #[cfg(bypass_io_native_dpdk)]
        assert_eq!(
            DpdkBackend::init(config()).unwrap_err(),
            DpdkError::RuntimeUnavailable {
                detail:
                    "native DPDK link flags are active, but the safe runtime adapter is not implemented"
            }
        );
    }

    fn sample_udp_packet() -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&[0, 1, 2, 3, 4, 5]);
        packet.extend_from_slice(&[6, 7, 8, 9, 10, 11]);
        packet.extend_from_slice(&0x0800u16.to_be_bytes());
        packet.extend_from_slice(&[
            0x45, 0, 0, 32, 0, 0, 0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 239, 1, 2, 3,
        ]);
        packet.extend_from_slice(&1234u16.to_be_bytes());
        packet.extend_from_slice(&9000u16.to_be_bytes());
        packet.extend_from_slice(&12u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(b"DATA");
        packet
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        use std::pin::pin;
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        fn wake(_: *const ()) {}
        fn wake_by_ref(_: *const ()) {}
        fn drop(_: *const ()) {}
        fn raw_waker() -> RawWaker {
            RawWaker::new(
                std::ptr::null(),
                &RawWakerVTable::new(clone, wake, wake_by_ref, drop),
            )
        }

        // Safety: the waker never dereferences the data pointer and is used only
        // for immediately-ready futures in these tests.
        let waker = unsafe { Waker::from_raw(raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut future = pin!(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test future unexpectedly returned Pending"),
        }
    }
}
