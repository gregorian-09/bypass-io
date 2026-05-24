use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;

use super::{HugeBuf, PooledBuf};

/// C-compatible scatter-gather vector.
///
/// This mirrors Linux `struct iovec`: a base pointer plus byte length. The base
/// pointer is mutable because Linux uses the same layout for read and write
/// vectors.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawIoVec {
    /// Base address of the buffer.
    pub iov_base: *mut c_void,
    /// Length of the buffer in bytes.
    pub iov_len: usize,
}

/// Immutable scatter-gather view tied to a borrowed buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoVec<'a> {
    raw: RawIoVec,
    _borrow: PhantomData<&'a [u8]>,
}

impl<'a> IoVec<'a> {
    /// Build an immutable I/O vector from a byte slice.
    #[must_use]
    pub fn from_slice(slice: &'a [u8]) -> Self {
        Self {
            raw: RawIoVec {
                iov_base: slice.as_ptr().cast_mut().cast::<c_void>(),
                iov_len: slice.len(),
            },
            _borrow: PhantomData,
        }
    }

    /// Build an immutable I/O vector from a huge buffer.
    #[must_use]
    pub fn from_huge_buf(buf: &'a HugeBuf) -> Self {
        Self::from_slice(buf.as_slice())
    }

    /// Build an immutable I/O vector from a pooled buffer.
    #[must_use]
    pub fn from_pooled_buf(buf: &'a PooledBuf) -> Self {
        Self::from_huge_buf(buf.buf())
    }

    /// Return the raw Linux-compatible vector.
    #[must_use]
    pub fn as_raw(self) -> RawIoVec {
        self.raw
    }

    /// Return the vector length in bytes.
    #[must_use]
    pub fn len(self) -> usize {
        self.raw.iov_len
    }

    /// Return true when the vector has zero length.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Mutable scatter-gather view tied to a mutably borrowed buffer.
#[derive(Debug, Eq, PartialEq)]
pub struct IoVecMut<'a> {
    raw: RawIoVec,
    _borrow: PhantomData<&'a mut [u8]>,
}

impl<'a> IoVecMut<'a> {
    /// Build a mutable I/O vector from a byte slice.
    #[must_use]
    pub fn from_mut_slice(slice: &'a mut [u8]) -> Self {
        Self {
            raw: RawIoVec {
                iov_base: slice.as_mut_ptr().cast::<c_void>(),
                iov_len: slice.len(),
            },
            _borrow: PhantomData,
        }
    }

    /// Build a mutable I/O vector from a huge buffer.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that no device, kernel operation, or other
    /// thread can access `buf` while the returned vector is used for mutable
    /// I/O.
    pub unsafe fn from_huge_buf(buf: &'a mut HugeBuf) -> Self {
        let ptr = buf.virt_addr();
        Self::from_parts(ptr, buf.len())
    }

    /// Build a mutable I/O vector from a pooled buffer.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that no device, kernel operation, or other
    /// thread can access `buf` while the returned vector is used for mutable
    /// I/O.
    pub unsafe fn from_pooled_buf(buf: &'a mut PooledBuf) -> Self {
        // Safety: forwarded to the caller of this unsafe constructor.
        unsafe { Self::from_huge_buf(buf.buf_mut()) }
    }

    /// Return the raw Linux-compatible vector.
    #[must_use]
    pub fn as_raw(&self) -> RawIoVec {
        self.raw
    }

    /// Return the vector length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.raw.iov_len
    }

    /// Return true when the vector has zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn from_parts(ptr: NonNull<u8>, len: usize) -> Self {
        Self {
            raw: RawIoVec {
                iov_base: ptr.as_ptr().cast::<c_void>(),
                iov_len: len,
            },
            _borrow: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IoVec;

    #[test]
    fn immutable_iovec_reports_slice_pointer_and_len() {
        let data = [1u8, 2, 3, 4];
        let iovec = IoVec::from_slice(&data);
        let raw = iovec.as_raw();

        assert_eq!(raw.iov_base, data.as_ptr().cast_mut().cast());
        assert_eq!(raw.iov_len, data.len());
        assert!(!iovec.is_empty());
    }

    #[test]
    fn mutable_iovec_reports_slice_pointer_and_len() {
        let mut data = [0u8; 8];
        let expected_ptr = data.as_mut_ptr().cast();
        let expected_len = data.len();
        let iovec = super::IoVecMut::from_mut_slice(&mut data);
        let raw = iovec.as_raw();

        assert_eq!(raw.iov_base, expected_ptr);
        assert_eq!(raw.iov_len, expected_len);
        assert!(!iovec.is_empty());
    }
}
