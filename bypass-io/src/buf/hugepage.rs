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
    phys: u64,
    len: usize,
    page_size: HugePageSize,
    uring_buf_index: Option<u32>,
}

// The mapping is exclusively owned by `HugeBuf`; moving it across threads does
// not invalidate the mapping. Shared references only expose immutable slices.
unsafe impl Send for HugeBuf {}
unsafe impl Sync for HugeBuf {}

impl HugeBuf {
    /// Allocate a new huge-page-backed buffer.
    ///
    /// `len` is rounded up to the requested huge-page size.
    ///
    /// # Errors
    ///
    /// Returns an OS error when the huge-page mapping, page lock, or physical
    /// address lookup fails. Linux systems usually require huge pages to be
    /// reserved before this function can succeed.
    pub fn alloc(len: usize, page_size: HugePageSize) -> io::Result<Self> {
        let len = round_up(len, page_size.bytes().get())?;
        let virt = mmap_huge(len, page_size)?;
        let phys = match virt_to_phys(virt.as_ptr()) {
            Ok(phys) => phys,
            Err(err) => {
                // Safety: `virt` came from `mmap_huge` with exactly `len` bytes.
                unsafe {
                    let _ = munlock(virt.as_ptr().cast::<c_void>(), len);
                    let _ = munmap(virt.as_ptr().cast::<c_void>(), len);
                }
                return Err(err);
            }
        };

        Ok(Self {
            virt,
            phys,
            len,
            page_size,
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

    /// Physical address of the first byte, as resolved from `/proc/self/pagemap`.
    #[must_use]
    pub fn phys_addr(&self) -> u64 {
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

    /// Fixed-buffer index assigned by an `io_uring` backend, if registered.
    #[must_use]
    pub fn uring_buf_index(&self) -> Option<u32> {
        self.uring_buf_index
    }
}

impl Drop for HugeBuf {
    fn drop(&mut self) {
        // Safety: `virt` was returned by `mmap` and has not been unmapped yet.
        unsafe {
            let _ = munlock(self.virt.as_ptr().cast::<c_void>(), self.len);
            let _ = munmap(self.virt.as_ptr().cast::<c_void>(), self.len);
        }
    }
}

fn mmap_huge(len: usize, size: HugePageSize) -> io::Result<NonNull<u8>> {
    // Safety: arguments follow the Linux `mmap(2)` contract.
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            size.mmap_flags(),
            -1,
            0,
        )
    };
    if ptr == MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    // Safety: `ptr` is a valid mapping of `len` bytes from the successful mmap.
    if unsafe { mlock(ptr.cast_const(), len) } != 0 {
        let err = io::Error::last_os_error();
        // Safety: `ptr` is still the live mapping returned above.
        unsafe {
            let _ = munmap(ptr, len);
        }
        return Err(err);
    }

    NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::other("mmap returned null"))
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
    use super::{round_up, HugePageSize};

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
}
