//! SPDK NVMe backend.
//!
//! Phase 2 establishes the safe Rust side of the SPDK backend: namespace
//! metadata, byte-offset to LBA conversion, DMA-buffer eligibility checks, queue
//! pair polling, and the [`IoBackend`] implementation. The actual C SPDK calls
//! are isolated behind a private runtime trait so this crate can still build on
//! machines where SPDK is not installed.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::backend::{BoxIoFuture, DeviceTarget, IoBackend};
use crate::buf::{HugeBufBacking, PooledBuf};

const COMPLETIONS_PER_POLL: u32 = 256;

/// Phase 2 SPDK backend.
#[derive(Clone)]
pub struct SpdkBackend {
    controller: NvmeController,
    namespaces: Arc<[NvmeNamespace]>,
    qpairs: Arc<[IoQueuePair]>,
    runtime: Arc<dyn SpdkRuntime>,
}

impl fmt::Debug for SpdkBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpdkBackend")
            .field("controller", &self.controller)
            .field("namespaces", &self.namespaces)
            .field("qpairs", &self.qpairs)
            .finish_non_exhaustive()
    }
}

impl SpdkBackend {
    /// Return the native SPDK runtime integration status for this build.
    #[must_use]
    #[cfg(bypass_io_native_spdk)]
    pub const fn native_status() -> SpdkNativeStatus {
        SpdkNativeStatus {
            linked: true,
            detail: "native SPDK link flags are active; native runtime adapter scaffold is compiled with I/O disabled",
        }
    }

    /// Return the native SPDK runtime integration status for this build.
    #[must_use]
    #[cfg(not(bypass_io_native_spdk))]
    pub const fn native_status() -> SpdkNativeStatus {
        SpdkNativeStatus {
            linked: false,
            detail: "native SPDK symbols are not linked; Rust validation runtime is active",
        }
    }

    /// Probe PCIe NVMe devices and initialize SPDK.
    ///
    /// # Errors
    ///
    /// Returns [`SpdkError::RuntimeUnavailable`] until the native SPDK runtime
    /// adapter is implemented. This keeps the `spdk` feature useful for API and
    /// validation work on machines without SPDK installed.
    #[cfg(bypass_io_native_spdk)]
    pub fn probe_and_init() -> Result<Self, SpdkError> {
        native::probe_and_init()
    }

    /// Probe PCIe NVMe devices and initialize SPDK.
    ///
    /// # Errors
    ///
    /// Returns [`SpdkError::RuntimeUnavailable`] until the native SPDK runtime
    /// adapter is implemented. This keeps the `spdk` feature useful for API and
    /// validation work on machines without SPDK installed.
    #[cfg(not(bypass_io_native_spdk))]
    pub fn probe_and_init() -> Result<Self, SpdkError> {
        Err(SpdkError::RuntimeUnavailable {
            detail: runtime_unavailable_detail(),
        })
    }

    /// Build a metadata-only backend that validates SPDK requests but returns
    /// [`SpdkError::RuntimeUnavailable`] at submission time.
    ///
    /// This is useful for configuration loading, API tests, and applications
    /// that want to validate namespace metadata before the native SPDK adapter
    /// is available.
    #[must_use]
    pub fn unavailable(
        controller: NvmeController,
        namespaces: Vec<NvmeNamespace>,
        qpairs: Vec<IoQueuePair>,
    ) -> Self {
        Self {
            controller,
            namespaces: namespaces.into(),
            qpairs: qpairs.into(),
            runtime: Arc::new(UnavailableSpdkRuntime),
        }
    }

    /// Return the controller descriptor.
    #[must_use]
    pub fn controller(&self) -> &NvmeController {
        &self.controller
    }

    /// Return discovered namespace descriptors.
    #[must_use]
    pub fn namespaces(&self) -> &[NvmeNamespace] {
        &self.namespaces
    }

    /// Return the sector size for a namespace.
    ///
    /// # Errors
    ///
    /// Returns [`SpdkError::NamespaceNotFound`] when `nsid` is not known.
    pub fn sector_size(&self, nsid: u32) -> Result<u32, SpdkError> {
        Ok(self.namespace(nsid)?.sector_size())
    }

    /// Return the maximum I/O size for a namespace.
    ///
    /// # Errors
    ///
    /// Returns [`SpdkError::NamespaceNotFound`] when `nsid` is not known.
    pub fn max_io_size(&self, nsid: u32) -> Result<u32, SpdkError> {
        Ok(self.namespace(nsid)?.max_io_size())
    }

    fn namespace(&self, nsid: u32) -> Result<&NvmeNamespace, SpdkError> {
        self.namespaces
            .iter()
            .find(|ns| ns.nsid == nsid)
            .ok_or(SpdkError::NamespaceNotFound { nsid })
    }

    fn qpair(&self) -> Result<&IoQueuePair, SpdkError> {
        self.qpairs.first().ok_or(SpdkError::NoQueuePairsConfigured)
    }

    fn prepare_io(
        &self,
        target: DeviceTarget,
        len: usize,
        offset: u64,
    ) -> Result<(&NvmeNamespace, &IoQueuePair, NvmeLbaRange), SpdkError> {
        let DeviceTarget::NvmeNs { nsid } = target else {
            return Err(SpdkError::InvalidTarget {
                expected: "DeviceTarget::NvmeNs",
            });
        };
        let namespace = self.namespace(nsid)?;
        let range = namespace.lba_range(offset, len)?;
        let qpair = self.qpair()?;
        Ok((namespace, qpair, range))
    }

    #[cfg(test)]
    fn with_runtime(
        controller: NvmeController,
        namespaces: Vec<NvmeNamespace>,
        qpairs: Vec<IoQueuePair>,
        runtime: Arc<dyn SpdkRuntime>,
    ) -> Self {
        Self {
            controller,
            namespaces: namespaces.into(),
            qpairs: qpairs.into(),
            runtime,
        }
    }
}

/// Native SPDK runtime status for the current build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpdkNativeStatus {
    /// Whether this build links a native SPDK runtime adapter.
    pub linked: bool,
    /// Human-readable status detail.
    pub detail: &'static str,
}

impl IoBackend for SpdkBackend {
    type Error = SpdkError;

    fn read<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a mut PooledBuf,
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let (namespace, qpair, range) = self.prepare_io(target, buf.len(), offset)?;
            let segment = SpdkBufferSegment::from_pooled(buf)?;
            self.runtime.read(namespace, qpair, segment, range)
        })
    }

    fn write<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a PooledBuf,
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let (namespace, qpair, range) = self.prepare_io(target, buf.len(), offset)?;
            let segment = SpdkBufferSegment::from_pooled(buf)?;
            self.runtime.write(namespace, qpair, segment, range)
        })
    }

    fn readv<'a>(
        &'a self,
        target: DeviceTarget,
        bufs: &'a mut [PooledBuf],
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let len = total_pooled_len(bufs.iter())?;
            let (namespace, qpair, range) = self.prepare_io(target, len, offset)?;
            let mut segments = spdk_segments(bufs.iter())?;
            self.runtime.readv(namespace, qpair, &mut segments, range)
        })
    }

    fn writev<'a>(
        &'a self,
        target: DeviceTarget,
        bufs: &'a [PooledBuf],
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let len = total_pooled_len(bufs.iter())?;
            let (namespace, qpair, range) = self.prepare_io(target, len, offset)?;
            let segments = spdk_segments(bufs.iter())?;
            self.runtime.writev(namespace, qpair, &segments, range)
        })
    }

    fn flush<'a>(&'a self, target: DeviceTarget) -> BoxIoFuture<'a, (), Self::Error> {
        Box::pin(async move {
            let DeviceTarget::NvmeNs { nsid } = target else {
                return Err(SpdkError::InvalidTarget {
                    expected: "DeviceTarget::NvmeNs",
                });
            };
            let namespace = self.namespace(nsid)?;
            let qpair = self.qpair()?;
            self.runtime.flush(namespace, qpair)
        })
    }

    fn poll_completions(&self) -> usize {
        self.qpairs
            .iter()
            .map(|qpair| {
                self.runtime
                    .process_completions(qpair, COMPLETIONS_PER_POLL)
            })
            .sum()
    }
}

/// SPDK NVMe controller descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NvmeController {
    name: String,
    pci_addr: Option<String>,
}

impl NvmeController {
    /// Create a controller descriptor.
    #[must_use]
    pub fn new(name: impl Into<String>, pci_addr: Option<String>) -> Self {
        Self {
            name: name.into(),
            pci_addr,
        }
    }

    /// Human-readable controller name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// PCI bus/device/function address, when known.
    #[must_use]
    pub fn pci_addr(&self) -> Option<&str> {
        self.pci_addr.as_deref()
    }
}

/// SPDK NVMe namespace descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NvmeNamespace {
    nsid: u32,
    sector_size: u32,
    capacity_sectors: u64,
    max_io_size: u32,
}

impl NvmeNamespace {
    /// Create a namespace descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`SpdkError::InvalidNamespace`] when any required namespace
    /// property is zero.
    pub fn new(
        nsid: u32,
        sector_size: u32,
        capacity_sectors: u64,
        max_io_size: u32,
    ) -> Result<Self, SpdkError> {
        if nsid == 0 {
            return Err(SpdkError::InvalidNamespace("namespace id must be non-zero"));
        }
        if sector_size == 0 {
            return Err(SpdkError::InvalidNamespace("sector size must be non-zero"));
        }
        if capacity_sectors == 0 {
            return Err(SpdkError::InvalidNamespace("capacity must be non-zero"));
        }
        if max_io_size == 0 {
            return Err(SpdkError::InvalidNamespace("max I/O size must be non-zero"));
        }
        Ok(Self {
            nsid,
            sector_size,
            capacity_sectors,
            max_io_size,
        })
    }

    /// NVMe namespace identifier.
    #[must_use]
    pub fn nsid(&self) -> u32 {
        self.nsid
    }

    /// Logical block size in bytes.
    #[must_use]
    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// Namespace capacity in logical blocks.
    #[must_use]
    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// Maximum I/O size in bytes.
    #[must_use]
    pub fn max_io_size(&self) -> u32 {
        self.max_io_size
    }

    /// Convert a byte offset and byte length into an NVMe LBA range.
    ///
    /// # Errors
    ///
    /// Returns an alignment, size, or bounds error when the range cannot be
    /// represented as namespace logical blocks.
    pub fn lba_range(&self, offset: u64, len: usize) -> Result<NvmeLbaRange, SpdkError> {
        if len == 0 {
            return Err(SpdkError::ZeroLengthIo);
        }
        if len > self.max_io_size as usize {
            return Err(SpdkError::IoTooLarge {
                len,
                max: self.max_io_size as usize,
            });
        }

        let sector = self.sector_size as u64;
        if !offset.is_multiple_of(sector) {
            return Err(SpdkError::UnalignedIo {
                offset,
                len,
                sector_size: self.sector_size,
            });
        }
        if !(len as u64).is_multiple_of(sector) {
            return Err(SpdkError::UnalignedIo {
                offset,
                len,
                sector_size: self.sector_size,
            });
        }

        let lba = offset / sector;
        let lba_count = (len as u64) / sector;
        let end = lba.checked_add(lba_count).ok_or(SpdkError::IoOutOfRange {
            lba,
            lba_count,
            capacity: self.capacity_sectors,
        })?;
        if end > self.capacity_sectors {
            return Err(SpdkError::IoOutOfRange {
                lba,
                lba_count,
                capacity: self.capacity_sectors,
            });
        }

        Ok(NvmeLbaRange { lba, lba_count })
    }
}

/// Logical block address range for one NVMe command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NvmeLbaRange {
    /// Starting logical block address.
    pub lba: u64,
    /// Number of logical blocks.
    pub lba_count: u64,
}

/// Queue pair descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IoQueuePair {
    id: usize,
}

impl IoQueuePair {
    /// Create a queue-pair descriptor.
    #[must_use]
    pub fn new(id: usize) -> Self {
        Self { id }
    }

    /// Queue-pair identifier local to the backend.
    #[must_use]
    pub fn id(&self) -> usize {
        self.id
    }
}

/// Errors produced by the SPDK backend.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SpdkError {
    /// Native SPDK runtime is not linked into this build.
    RuntimeUnavailable {
        /// Human-readable detail.
        detail: &'static str,
    },
    /// A non-NVMe target was passed to the SPDK backend.
    InvalidTarget {
        /// Expected target kind.
        expected: &'static str,
    },
    /// Namespace metadata was invalid.
    InvalidNamespace(&'static str),
    /// Requested namespace was not discovered.
    NamespaceNotFound {
        /// Missing namespace id.
        nsid: u32,
    },
    /// No queue pair is available for submission.
    NoQueuePairsConfigured,
    /// Zero-length I/O is not submitted to NVMe.
    ZeroLengthIo,
    /// I/O offset or length is not aligned to namespace sector size.
    UnalignedIo {
        /// Requested byte offset.
        offset: u64,
        /// Requested byte length.
        len: usize,
        /// Namespace sector size.
        sector_size: u32,
    },
    /// Requested I/O is larger than the namespace limit.
    IoTooLarge {
        /// Requested byte length.
        len: usize,
        /// Maximum byte length.
        max: usize,
    },
    /// Requested I/O extends beyond namespace capacity.
    IoOutOfRange {
        /// Starting logical block address.
        lba: u64,
        /// Number of logical blocks.
        lba_count: u64,
        /// Namespace capacity in logical blocks.
        capacity: u64,
    },
    /// The supplied buffer is not suitable for SPDK DMA.
    DmaBufferUnavailable {
        /// Human-readable detail.
        detail: &'static str,
    },
    /// Vector lengths overflowed `usize`.
    LengthOverflow,
    /// SPDK submission returned a negative or non-zero status code.
    SubmitFailed {
        /// SPDK return code.
        rc: i32,
    },
    /// NVMe completion reported an error status.
    CompletionFailed {
        /// Completion status code.
        status: u16,
    },
}

impl fmt::Display for SpdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable { detail } => write!(f, "SPDK runtime unavailable: {detail}"),
            Self::InvalidTarget { expected } => write!(f, "SPDK backend requires {expected}"),
            Self::InvalidNamespace(detail) => write!(f, "invalid NVMe namespace: {detail}"),
            Self::NamespaceNotFound { nsid } => write!(f, "NVMe namespace {nsid} was not found"),
            Self::NoQueuePairsConfigured => write!(f, "no SPDK queue pairs are configured"),
            Self::ZeroLengthIo => write!(f, "zero-length NVMe I/O is invalid"),
            Self::UnalignedIo {
                offset,
                len,
                sector_size,
            } => write!(
                f,
                "I/O offset {offset} and length {len} must align to sector size {sector_size}"
            ),
            Self::IoTooLarge { len, max } => {
                write!(f, "I/O length {len} exceeds namespace maximum {max}")
            }
            Self::IoOutOfRange {
                lba,
                lba_count,
                capacity,
            } => write!(
                f,
                "LBA range {lba}..{} exceeds namespace capacity {capacity}",
                lba.saturating_add(*lba_count)
            ),
            Self::DmaBufferUnavailable { detail } => {
                write!(f, "buffer is not suitable for SPDK DMA: {detail}")
            }
            Self::LengthOverflow => write!(f, "I/O vector lengths overflowed usize"),
            Self::SubmitFailed { rc } => write!(f, "SPDK submission failed with rc={rc}"),
            Self::CompletionFailed { status } => {
                write!(f, "SPDK completion failed with status={status}")
            }
        }
    }
}

impl Error for SpdkError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SpdkBufferSegment {
    _addr: usize,
    len: usize,
}

impl SpdkBufferSegment {
    fn from_pooled(buf: &PooledBuf) -> Result<Self, SpdkError> {
        let huge = buf.buf();
        if matches!(huge.backing(), HugeBufBacking::PageableFallback) {
            return Err(SpdkError::DmaBufferUnavailable {
                detail: "mapping is pageable fallback memory",
            });
        }
        if !huge.is_page_locked() {
            return Err(SpdkError::DmaBufferUnavailable {
                detail: "mapping is not locked with mlock",
            });
        }
        if huge.phys_addr().is_none() {
            return Err(SpdkError::DmaBufferUnavailable {
                detail: "physical address is not visible to userspace",
            });
        }

        Ok(Self {
            _addr: huge.virt_addr().as_ptr() as usize,
            len: huge.len(),
        })
    }

    #[cfg(test)]
    fn as_slice(&self) -> &[u8] {
        // Safety: `from_pooled` constructs segments only from a live `PooledBuf`
        // borrow. Test runtimes call this before the backend future resolves.
        unsafe { std::slice::from_raw_parts(self._addr as *const u8, self.len) }
    }

    #[cfg(test)]
    fn as_mut_slice(&mut self) -> &mut [u8] {
        // Safety: read paths create the segment from `&mut PooledBuf`, and the
        // test runtime completes synchronously before the borrow is released.
        unsafe { std::slice::from_raw_parts_mut(self._addr as *mut u8, self.len) }
    }
}

trait SpdkRuntime: Send + Sync + 'static {
    fn read(
        &self,
        namespace: &NvmeNamespace,
        qpair: &IoQueuePair,
        segment: SpdkBufferSegment,
        range: NvmeLbaRange,
    ) -> Result<usize, SpdkError>;

    fn write(
        &self,
        namespace: &NvmeNamespace,
        qpair: &IoQueuePair,
        segment: SpdkBufferSegment,
        range: NvmeLbaRange,
    ) -> Result<usize, SpdkError>;

    fn readv(
        &self,
        namespace: &NvmeNamespace,
        qpair: &IoQueuePair,
        segments: &mut [SpdkBufferSegment],
        range: NvmeLbaRange,
    ) -> Result<usize, SpdkError>;

    fn writev(
        &self,
        namespace: &NvmeNamespace,
        qpair: &IoQueuePair,
        segments: &[SpdkBufferSegment],
        range: NvmeLbaRange,
    ) -> Result<usize, SpdkError>;

    fn flush(&self, namespace: &NvmeNamespace, qpair: &IoQueuePair) -> Result<(), SpdkError>;

    fn process_completions(&self, qpair: &IoQueuePair, max_completions: u32) -> usize;
}

struct UnavailableSpdkRuntime;

impl SpdkRuntime for UnavailableSpdkRuntime {
    fn read(
        &self,
        _namespace: &NvmeNamespace,
        _qpair: &IoQueuePair,
        _segment: SpdkBufferSegment,
        _range: NvmeLbaRange,
    ) -> Result<usize, SpdkError> {
        Err(runtime_unavailable())
    }

    fn write(
        &self,
        _namespace: &NvmeNamespace,
        _qpair: &IoQueuePair,
        _segment: SpdkBufferSegment,
        _range: NvmeLbaRange,
    ) -> Result<usize, SpdkError> {
        Err(runtime_unavailable())
    }

    fn readv(
        &self,
        _namespace: &NvmeNamespace,
        _qpair: &IoQueuePair,
        _segments: &mut [SpdkBufferSegment],
        _range: NvmeLbaRange,
    ) -> Result<usize, SpdkError> {
        Err(runtime_unavailable())
    }

    fn writev(
        &self,
        _namespace: &NvmeNamespace,
        _qpair: &IoQueuePair,
        _segments: &[SpdkBufferSegment],
        _range: NvmeLbaRange,
    ) -> Result<usize, SpdkError> {
        Err(runtime_unavailable())
    }

    fn flush(&self, _namespace: &NvmeNamespace, _qpair: &IoQueuePair) -> Result<(), SpdkError> {
        Err(runtime_unavailable())
    }

    fn process_completions(&self, _qpair: &IoQueuePair, _max_completions: u32) -> usize {
        0
    }
}

fn runtime_unavailable() -> SpdkError {
    SpdkError::RuntimeUnavailable {
        detail: runtime_unavailable_detail(),
    }
}

#[cfg(bypass_io_native_spdk)]
const fn runtime_unavailable_detail() -> &'static str {
    "native SPDK adapter scaffold is compiled, but SPDK I/O calls are disabled pending safety audit"
}

#[cfg(not(bypass_io_native_spdk))]
const fn runtime_unavailable_detail() -> &'static str {
    "native SPDK runtime is not linked"
}

fn spdk_segments<'a>(
    bufs: impl Iterator<Item = &'a PooledBuf>,
) -> Result<Vec<SpdkBufferSegment>, SpdkError> {
    bufs.map(SpdkBufferSegment::from_pooled).collect()
}

#[cfg(test)]
fn total_len(segments: &[SpdkBufferSegment]) -> Result<usize, SpdkError> {
    segments.iter().try_fold(0usize, |total, segment| {
        total
            .checked_add(segment.len)
            .ok_or(SpdkError::LengthOverflow)
    })
}

fn total_pooled_len<'a>(mut bufs: impl Iterator<Item = &'a PooledBuf>) -> Result<usize, SpdkError> {
    bufs.try_fold(0usize, |total, buf| {
        total
            .checked_add(buf.len())
            .ok_or(SpdkError::LengthOverflow)
    })
}

#[cfg(bypass_io_native_spdk)]
mod native {
    use super::{
        runtime_unavailable, IoQueuePair, NvmeNamespace, SpdkBackend, SpdkBufferSegment, SpdkError,
        SpdkRuntime, UnavailableSpdkRuntime,
    };
    use crate::ffi::spdk_sys;

    /// Native SPDK adapter scaffold.
    ///
    /// This type exists only when the build script has enabled
    /// `bypass_io_native_spdk`. It documents the exact place where future SPDK C
    /// calls will live, but it deliberately delegates every operation to the
    /// unavailable runtime until the unsafe completion and DMA ownership model
    /// is implemented and tested on hardware.
    pub(super) struct NativeSpdkRuntime {
        fallback: UnavailableSpdkRuntime,
    }

    impl NativeSpdkRuntime {
        pub(super) const fn new() -> Self {
            Self {
                fallback: UnavailableSpdkRuntime,
            }
        }

        pub(super) const fn required_symbols() -> &'static [&'static str] {
            &[
                "spdk_nvme_probe",
                "spdk_nvme_ctrlr_alloc_io_qpair",
                "spdk_nvme_ns_cmd_read",
                "spdk_nvme_ns_cmd_write",
                "spdk_nvme_ns_cmd_flush",
                "spdk_nvme_qpair_process_completions",
            ]
        }
    }

    impl SpdkRuntime for NativeSpdkRuntime {
        fn read(
            &self,
            namespace: &NvmeNamespace,
            qpair: &IoQueuePair,
            segment: SpdkBufferSegment,
            range: super::NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            self.fallback.read(namespace, qpair, segment, range)
        }

        fn write(
            &self,
            namespace: &NvmeNamespace,
            qpair: &IoQueuePair,
            segment: SpdkBufferSegment,
            range: super::NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            self.fallback.write(namespace, qpair, segment, range)
        }

        fn readv(
            &self,
            namespace: &NvmeNamespace,
            qpair: &IoQueuePair,
            segments: &mut [SpdkBufferSegment],
            range: super::NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            self.fallback.readv(namespace, qpair, segments, range)
        }

        fn writev(
            &self,
            namespace: &NvmeNamespace,
            qpair: &IoQueuePair,
            segments: &[SpdkBufferSegment],
            range: super::NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            self.fallback.writev(namespace, qpair, segments, range)
        }

        fn flush(&self, namespace: &NvmeNamespace, qpair: &IoQueuePair) -> Result<(), SpdkError> {
            self.fallback.flush(namespace, qpair)
        }

        fn process_completions(&self, qpair: &IoQueuePair, max_completions: u32) -> usize {
            self.fallback.process_completions(qpair, max_completions)
        }
    }

    pub(super) fn probe_and_init() -> Result<SpdkBackend, SpdkError> {
        let _runtime = NativeSpdkRuntime::new();
        let _symbols = NativeSpdkRuntime::required_symbols();
        let _ffi_markers = (
            core::mem::size_of::<spdk_sys::spdk_nvme_ctrlr>(),
            core::mem::size_of::<spdk_sys::spdk_nvme_ns>(),
            core::mem::size_of::<spdk_sys::spdk_nvme_qpair>(),
        );
        Err(runtime_unavailable())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{remove_file, File, OpenOptions};
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        IoQueuePair, NvmeController, NvmeLbaRange, NvmeNamespace, SpdkBackend, SpdkBufferSegment,
        SpdkError, SpdkRuntime,
    };
    use crate::backend::{DeviceTarget, IoBackend};
    use crate::buf::{BufPool, HugePageSize};

    #[derive(Default)]
    struct RecordingRuntime {
        writes: Mutex<Vec<(u32, usize, NvmeLbaRange)>>,
        flushes: AtomicUsize,
        polls: AtomicUsize,
    }

    impl SpdkRuntime for RecordingRuntime {
        fn read(
            &self,
            _namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segment: SpdkBufferSegment,
            _range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            Ok(segment.len)
        }

        fn write(
            &self,
            namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segment: SpdkBufferSegment,
            range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            self.writes
                .lock()
                .unwrap()
                .push((namespace.nsid(), segment.len, range));
            Ok(segment.len)
        }

        fn readv(
            &self,
            _namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segments: &mut [SpdkBufferSegment],
            _range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            super::total_len(segments)
        }

        fn writev(
            &self,
            _namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segments: &[SpdkBufferSegment],
            _range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            super::total_len(segments)
        }

        fn flush(&self, _namespace: &NvmeNamespace, _qpair: &IoQueuePair) -> Result<(), SpdkError> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn process_completions(&self, _qpair: &IoQueuePair, max_completions: u32) -> usize {
            self.polls
                .fetch_add(max_completions as usize, Ordering::Relaxed);
            3
        }
    }

    struct FileBackedSpdkRuntime {
        file: Mutex<File>,
        flushes: AtomicUsize,
    }

    impl FileBackedSpdkRuntime {
        fn new(file: File) -> Self {
            Self {
                file: Mutex::new(file),
                flushes: AtomicUsize::new(0),
            }
        }

        fn range_offset(namespace: &NvmeNamespace, range: NvmeLbaRange) -> Result<u64, SpdkError> {
            range
                .lba
                .checked_mul(namespace.sector_size() as u64)
                .ok_or(SpdkError::SubmitFailed { rc: -1 })
        }

        fn range_len(namespace: &NvmeNamespace, range: NvmeLbaRange) -> Result<usize, SpdkError> {
            let bytes = range
                .lba_count
                .checked_mul(namespace.sector_size() as u64)
                .ok_or(SpdkError::LengthOverflow)?;
            usize::try_from(bytes).map_err(|_| SpdkError::LengthOverflow)
        }

        fn io_error() -> SpdkError {
            SpdkError::SubmitFailed { rc: -1 }
        }
    }

    impl SpdkRuntime for FileBackedSpdkRuntime {
        fn read(
            &self,
            namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            mut segment: SpdkBufferSegment,
            range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            let offset = Self::range_offset(namespace, range)?;
            let len = Self::range_len(namespace, range)?;
            let slice = segment
                .as_mut_slice()
                .get_mut(..len)
                .ok_or(SpdkError::LengthOverflow)?;
            let file = self.file.lock().unwrap();
            file.read_exact_at(slice, offset)
                .map_err(|_| Self::io_error())?;
            Ok(len)
        }

        fn write(
            &self,
            namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segment: SpdkBufferSegment,
            range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            let offset = Self::range_offset(namespace, range)?;
            let len = Self::range_len(namespace, range)?;
            let slice = segment
                .as_slice()
                .get(..len)
                .ok_or(SpdkError::LengthOverflow)?;
            let file = self.file.lock().unwrap();
            file.write_all_at(slice, offset)
                .map_err(|_| Self::io_error())?;
            Ok(len)
        }

        fn readv(
            &self,
            namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segments: &mut [SpdkBufferSegment],
            range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            let mut offset = Self::range_offset(namespace, range)?;
            let mut total = 0usize;
            let file = self.file.lock().unwrap();
            for segment in segments {
                let slice = segment.as_mut_slice();
                file.read_exact_at(slice, offset)
                    .map_err(|_| Self::io_error())?;
                offset = offset
                    .checked_add(slice.len() as u64)
                    .ok_or(SpdkError::LengthOverflow)?;
                total = total
                    .checked_add(slice.len())
                    .ok_or(SpdkError::LengthOverflow)?;
            }
            Ok(total)
        }

        fn writev(
            &self,
            namespace: &NvmeNamespace,
            _qpair: &IoQueuePair,
            segments: &[SpdkBufferSegment],
            range: NvmeLbaRange,
        ) -> Result<usize, SpdkError> {
            let mut offset = Self::range_offset(namespace, range)?;
            let mut total = 0usize;
            let file = self.file.lock().unwrap();
            for segment in segments {
                let slice = segment.as_slice();
                file.write_all_at(slice, offset)
                    .map_err(|_| Self::io_error())?;
                offset = offset
                    .checked_add(slice.len() as u64)
                    .ok_or(SpdkError::LengthOverflow)?;
                total = total
                    .checked_add(slice.len())
                    .ok_or(SpdkError::LengthOverflow)?;
            }
            Ok(total)
        }

        fn flush(&self, _namespace: &NvmeNamespace, _qpair: &IoQueuePair) -> Result<(), SpdkError> {
            self.file
                .lock()
                .unwrap()
                .sync_all()
                .map_err(|_| Self::io_error())?;
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn process_completions(&self, _qpair: &IoQueuePair, _max_completions: u32) -> usize {
            0
        }
    }

    fn namespace() -> NvmeNamespace {
        NvmeNamespace::new(1, 512, 4096, 4096).unwrap()
    }

    fn file_backed_namespace() -> NvmeNamespace {
        NvmeNamespace::new(1, 512, 8192, 2 * 1024 * 1024).unwrap()
    }

    fn backend(runtime: Arc<dyn SpdkRuntime>) -> SpdkBackend {
        SpdkBackend::with_runtime(
            NvmeController::new("test", Some("0000:01:00.0".to_string())),
            vec![namespace()],
            vec![IoQueuePair::new(0), IoQueuePair::new(1)],
            runtime,
        )
    }

    fn backend_with_namespace(
        namespace: NvmeNamespace,
        runtime: Arc<dyn SpdkRuntime>,
    ) -> SpdkBackend {
        SpdkBackend::with_runtime(
            NvmeController::new("test", Some("0000:01:00.0".to_string())),
            vec![namespace],
            vec![IoQueuePair::new(0)],
            runtime,
        )
    }

    fn temp_file(prefix: &str, len: u64) -> (File, PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("bypass-io-{prefix}-{}-{stamp}", std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(len).unwrap();
        (file, path)
    }

    #[test]
    fn namespace_converts_byte_ranges_to_lbas() {
        let ns = namespace();
        assert_eq!(
            ns.lba_range(1024, 1536).unwrap(),
            NvmeLbaRange {
                lba: 2,
                lba_count: 3
            }
        );
    }

    #[test]
    fn namespace_rejects_unaligned_io() {
        let ns = namespace();
        assert_eq!(
            ns.lba_range(1, 512).unwrap_err(),
            SpdkError::UnalignedIo {
                offset: 1,
                len: 512,
                sector_size: 512
            }
        );
        assert_eq!(
            ns.lba_range(0, 513).unwrap_err(),
            SpdkError::UnalignedIo {
                offset: 0,
                len: 513,
                sector_size: 512
            }
        );
    }

    #[test]
    fn namespace_rejects_oversized_and_out_of_range_io() {
        let ns = namespace();
        assert_eq!(
            ns.lba_range(0, 8192).unwrap_err(),
            SpdkError::IoTooLarge {
                len: 8192,
                max: 4096
            }
        );
        assert_eq!(
            ns.lba_range(4095 * 512, 1024).unwrap_err(),
            SpdkError::IoOutOfRange {
                lba: 4095,
                lba_count: 2,
                capacity: 4096
            }
        );
    }

    #[test]
    fn backend_rejects_non_nvme_targets_before_submission() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(runtime);
        let pool = BufPool::new(1, 512, HugePageSize::Mib2).unwrap();
        let buf = pool.acquire().unwrap();

        let err = block_on(backend.write(DeviceTarget::Fd(1), &buf, 0)).unwrap_err();
        assert_eq!(
            err,
            SpdkError::InvalidTarget {
                expected: "DeviceTarget::NvmeNs"
            }
        );
    }

    #[test]
    fn backend_poll_drives_all_configured_qpairs() {
        let runtime = Arc::new(RecordingRuntime::default());
        let backend = backend(Arc::clone(&runtime) as Arc<dyn SpdkRuntime>);

        assert_eq!(backend.poll_completions(), 6);
        assert_eq!(
            runtime.polls.load(Ordering::Relaxed),
            super::COMPLETIONS_PER_POLL as usize * 2
        );
    }

    #[test]
    fn file_backed_runtime_moves_bytes_through_spdk_backend_pipeline() {
        let (file, path) = temp_file("spdk", 4 * 1024 * 1024);
        let runtime = Arc::new(FileBackedSpdkRuntime::new(file));
        let backend = backend_with_namespace(
            file_backed_namespace(),
            Arc::clone(&runtime) as Arc<dyn SpdkRuntime>,
        );
        let pool = BufPool::new(2, 512, HugePageSize::Mib2).unwrap();
        let mut write_buf = pool.acquire().unwrap();
        let mut read_buf = pool.acquire().unwrap();

        let write_len = write_buf.len();
        {
            // Safety: the test owns both checked-out buffers and no runtime has
            // a pending operation touching them while the slices are live.
            let write_slice = unsafe { write_buf.buf_mut().as_slice_mut() };
            let read_slice = unsafe { read_buf.buf_mut().as_slice_mut() };
            for (idx, byte) in write_slice.iter_mut().enumerate() {
                *byte = (idx % 251) as u8;
            }
            read_slice.fill(0);
        }

        let target = DeviceTarget::NvmeNs { nsid: 1 };
        match block_on(backend.write(target.clone(), &write_buf, 512)) {
            Ok(written) => assert_eq!(written, write_len),
            Err(SpdkError::DmaBufferUnavailable { .. }) => {
                remove_file(path).ok();
                return;
            }
            Err(err) => panic!("unexpected SPDK write error: {err:?}"),
        }

        let read = block_on(backend.read(target.clone(), &mut read_buf, 512)).unwrap();
        assert_eq!(read, write_len);
        block_on(backend.flush(target)).unwrap();
        assert_eq!(runtime.flushes.load(Ordering::Relaxed), 1);
        assert_eq!(read_buf.buf().as_slice(), write_buf.buf().as_slice());

        remove_file(path).ok();
    }

    #[test]
    fn probe_reports_unavailable_without_native_spdk_runtime() {
        #[cfg(not(bypass_io_native_spdk))]
        assert_eq!(
            SpdkBackend::native_status(),
            super::SpdkNativeStatus {
                linked: false,
                detail: "native SPDK symbols are not linked; Rust validation runtime is active"
            }
        );
        #[cfg(bypass_io_native_spdk)]
        assert_eq!(
            SpdkBackend::native_status(),
            super::SpdkNativeStatus {
                linked: true,
                detail: "native SPDK link flags are active; native runtime adapter scaffold is compiled with I/O disabled"
            }
        );
        #[cfg(not(bypass_io_native_spdk))]
        assert_eq!(
            SpdkBackend::probe_and_init().unwrap_err(),
            SpdkError::RuntimeUnavailable {
                detail: "native SPDK runtime is not linked"
            }
        );
        #[cfg(bypass_io_native_spdk)]
        assert_eq!(
            SpdkBackend::probe_and_init().unwrap_err(),
            SpdkError::RuntimeUnavailable {
                detail:
                    "native SPDK adapter scaffold is compiled, but SPDK I/O calls are disabled pending safety audit"
            }
        );
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
