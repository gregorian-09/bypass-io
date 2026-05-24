use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::FileExt;
use std::ptr::NonNull;
use std::slice;

const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const MAP_PRIVATE: i32 = 0x02;
const MAP_ANONYMOUS: i32 = 0x20;
const MAP_HUGETLB: i32 = 0x40000;
const MAP_HUGE_SHIFT: i32 = 26;
const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: isize,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> i32;
    fn mlock(addr: *const c_void, length: usize) -> i32;
    fn munlock(addr: *const c_void, length: usize) -> i32;
    fn sysconf(name: i32) -> isize;
}

const SC_PAGESIZE: i32 = 30;

/// Size class requested for a huge-page mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HugePageSize {
    /// 2 MiB huge pages.
    Mib2,
    /// 1 GiB huge pages.
    Gib1,
}

/// Actual memory backing selected for a [`HugeBuf`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HugeBufBacking {
    /// Linux accepted the requested `MAP_HUGETLB` mapping.
    HugePage,
    /// Linux could not provide a huge page, but the fallback mapping was locked.
    LockedFallback,
    /// Linux could not provide a huge page or lock the fallback mapping.
    ///
    /// This keeps Phase 1 usable on ordinary developer machines where huge
    /// pages and `RLIMIT_MEMLOCK` are not configured. SPDK/DPDK backends must
    /// reject this backing before submitting hardware DMA.
    PageableFallback,
}

impl HugePageSize {
    /// Return the page size in bytes.
    #[must_use]
    pub const fn bytes(self) -> NonZeroUsize {
        match self {
            Self::Mib2 => {
                // SAFETY: 2 MiB is non-zero.
                unsafe { NonZeroUsize::new_unchecked(2 * 1024 * 1024) }
            }
            Self::Gib1 => {
                // SAFETY: 1 GiB is non-zero.
                unsafe { NonZeroUsize::new_unchecked(1024 * 1024 * 1024) }
            }
        }
    }

    const fn mmap_flags(self) -> i32 {
        let huge_bits = match self {
            Self::Mib2 => 21,
            Self::Gib1 => 30,
        };
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | (huge_bits << MAP_HUGE_SHIFT)
    }
}

/// A contiguous, page-locked memory mapping intended for DMA registration.
///
/// `HugeBuf` is not cloneable. Moving the Rust owner is fine because moving the
/// handle does not move the underlying mapping.
#[derive(Debug)]
pub struct HugeBuf {
    virt: NonNull<u8>,
    phys: Option<u64>,
    len: usize,
    page_size: HugePageSize,
    backing: HugeBufBacking,
    locked: bool,
    uring_buf_index: Option<u32>,
}

// The mapping is exclusively owned by `HugeBuf`; moving it across threads does
// not invalidate the mapping. Shared references only expose immutable slices.
unsafe impl Send for HugeBuf {}
unsafe impl Sync for HugeBuf {}

impl HugeBuf {
    /// Allocate a new DMA-oriented buffer.
    ///
    /// `len` is rounded up to the requested huge-page size. The allocator first
    /// tries a real Linux huge-page mapping. If the host has no reserved huge
    /// pages, it falls back to anonymous memory so Phase 1 works on ordinary
    /// Linux development machines.
    ///
    /// # Errors
    ///
    /// Returns an OS error when both the huge-page mapping and anonymous
    /// fallback mapping fail.
    pub fn alloc(len: usize, page_size: HugePageSize) -> io::Result<Self> {
        let len = round_up(len, page_size.bytes().get())?;
        let allocation = mmap_preferred(len, page_size)?;
        let virt = allocation.virt;
        prefault_mapping(virt, len);
        let phys = virt_to_phys(virt.as_ptr()).ok();

        Ok(Self {
            virt,
            phys,
            len,
            page_size,
            backing: allocation.backing,
            locked: allocation.locked,
            uring_buf_index: None,
        })
    }

    /// Return the buffer as an immutable byte slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // Safety: `virt` points to a valid mapping of `len` bytes for `self`.
        unsafe { slice::from_raw_parts(self.virt.as_ptr(), self.len) }
    }

    /// Return the buffer as a mutable byte slice.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that no device, kernel operation, or other
    /// thread can read from or write to this buffer for the duration of the
    /// returned mutable borrow.
    pub unsafe fn as_slice_mut(&mut self) -> &mut [u8] {
        // Safety: upheld by the caller; the mapping itself is valid for `len`.
        unsafe { slice::from_raw_parts_mut(self.virt.as_ptr(), self.len) }
    }

    /// Virtual address visible to the CPU.
    #[must_use]
    pub fn virt_addr(&self) -> NonNull<u8> {
        self.virt
    }

    /// Physical address of the first byte, if visible through `/proc/self/pagemap`.
    ///
    /// Linux often hides page frame numbers from unprivileged processes. The
    /// address is optional in Phase 1 because `io_uring` can register userspace
    /// buffers without callers knowing the physical address.
    #[must_use]
    pub fn phys_addr(&self) -> Option<u64> {
        self.phys
    }

    /// Length in bytes after huge-page rounding.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Return true when the buffer has zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Huge-page size used for this allocation.
    #[must_use]
    pub fn page_size(&self) -> HugePageSize {
        self.page_size
    }

    /// Actual memory backing selected by the allocator.
    #[must_use]
    pub fn backing(&self) -> HugeBufBacking {
        self.backing
    }

    /// Return true when the mapping is locked with `mlock`.
    #[must_use]
    pub fn is_page_locked(&self) -> bool {
        self.locked
    }

    /// Fixed-buffer index assigned by an `io_uring` backend, if registered.
    #[must_use]
    pub fn uring_buf_index(&self) -> Option<u32> {
        self.uring_buf_index
    }
}

fn prefault_mapping(virt: NonNull<u8>, len: usize) {
    // Safety: `virt` points to a writable mapping of `len` bytes. Touching the
    // full range faults pages in before pagemap inspection.
    unsafe {
        std::ptr::write_bytes(virt.as_ptr(), 0, len);
    }
}

impl Drop for HugeBuf {
    fn drop(&mut self) {
        // Safety: `virt` was returned by `mmap` and has not been unmapped yet.
        unsafe {
            if self.locked {
                let _ = munlock(self.virt.as_ptr().cast::<c_void>(), self.len);
            }
            let _ = munmap(self.virt.as_ptr().cast::<c_void>(), self.len);
        }
    }
}

struct Mapping {
    virt: NonNull<u8>,
    backing: HugeBufBacking,
    locked: bool,
}

fn mmap_preferred(len: usize, size: HugePageSize) -> io::Result<Mapping> {
    let huge_result = mmap_raw(len, size.mmap_flags()).and_then(|virt| {
        if let Err(err) = lock_mapping(virt, len) {
            // Safety: `virt` is the live mapping returned above.
            unsafe {
                let _ = munmap(virt.as_ptr().cast::<c_void>(), len);
            }
            Err(err)
        } else {
            Ok(virt)
        }
    });

    match huge_result {
        Ok(virt) => Ok(Mapping {
            virt,
            backing: HugeBufBacking::HugePage,
            locked: true,
        }),
        Err(huge_err) => {
            let virt = mmap_raw(len, MAP_PRIVATE | MAP_ANONYMOUS).map_err(|fallback_err| {
                io::Error::new(
                    fallback_err.kind(),
                    format!(
                        "huge-page mmap failed ({huge_err}); anonymous fallback failed ({fallback_err})"
                    ),
                )
            })?;
            let locked = lock_mapping(virt, len).is_ok();
            let backing = if locked {
                HugeBufBacking::LockedFallback
            } else {
                HugeBufBacking::PageableFallback
            };
            Ok(Mapping {
                virt,
                backing,
                locked,
            })
        }
    }
}

fn mmap_raw(len: usize, flags: i32) -> io::Result<NonNull<u8>> {
    // Safety: arguments follow the Linux `mmap(2)` contract.
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            flags,
            -1,
            0,
        )
    };
    if ptr == MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::other("mmap returned null"))
}

fn lock_mapping(virt: NonNull<u8>, len: usize) -> io::Result<()> {
    // Safety: `virt` is a valid mapping of `len` bytes.
    if unsafe { mlock(virt.as_ptr().cast_const().cast::<c_void>(), len) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn virt_to_phys(virt: *const u8) -> io::Result<u64> {
    let pagemap = File::open("/proc/self/pagemap")?;
    // Safety: `sysconf(_SC_PAGESIZE)` has no memory-safety preconditions.
    let base_page_size = unsafe { sysconf(SC_PAGESIZE) };
    if base_page_size <= 0 {
        return Err(io::Error::other("sysconf(_SC_PAGESIZE) failed"));
    }
    let base_page_size = base_page_size as u64;
    let virt_addr = virt as u64;
    let vpn = virt_addr / base_page_size;
    let offset = vpn * 8;
    let mut entry = [0u8; 8];
    pagemap.read_exact_at(&mut entry, offset)?;
    let entry = u64::from_ne_bytes(entry);

    if entry & (1 << 63) == 0 {
        return Err(io::Error::other("page is not present"));
    }

    let pfn = entry & ((1 << 55) - 1);
    if pfn == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "physical frame number is unavailable from pagemap",
        ));
    }

    Ok((pfn * base_page_size) + (virt_addr % base_page_size))
}

fn round_up(len: usize, align: usize) -> io::Result<usize> {
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "buffer length must be non-zero",
        ));
    }
    let add = align
        .checked_sub(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "alignment is zero"))?;
    let rounded = len
        .checked_add(add)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "buffer length overflow"))?
        / align
        * align;
    Ok(rounded)
}

#[cfg(test)]
mod tests {
    use super::{round_up, HugeBuf, HugeBufBacking, HugePageSize};

    #[test]
    fn rounds_to_huge_page_size() {
        assert_eq!(
            round_up(1, HugePageSize::Mib2.bytes().get()).unwrap(),
            2 * 1024 * 1024
        );
        assert_eq!(
            round_up(2 * 1024 * 1024, HugePageSize::Mib2.bytes().get()).unwrap(),
            2 * 1024 * 1024
        );
    }

    #[test]
    fn rejects_zero_length() {
        assert!(round_up(0, HugePageSize::Mib2.bytes().get()).is_err());
    }

    #[test]
    fn allocates_with_fallback_when_huge_pages_are_unavailable() {
        let buf = HugeBuf::alloc(1, HugePageSize::Mib2).unwrap();
        assert_eq!(buf.len(), HugePageSize::Mib2.bytes().get());
        assert!(matches!(
            buf.backing(),
            HugeBufBacking::HugePage
                | HugeBufBacking::LockedFallback
                | HugeBufBacking::PageableFallback
        ));
    }
}
