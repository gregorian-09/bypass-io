use std::io;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_queue::SegQueue;

use super::{HugeBuf, HugePageSize};

/// Fixed-size pool of reusable [`HugeBuf`] values.
#[derive(Debug)]
pub struct BufPool {
    free: Arc<SegQueue<HugeBuf>>,
    available: Arc<AtomicUsize>,
    buf_size: usize,
    page_size: HugePageSize,
}

impl BufPool {
    /// Allocate `count` buffers, each rounded to `buf_size`.
    ///
    /// # Errors
    ///
    /// Returns the first allocation error reported by [`HugeBuf::alloc`].
    pub fn new(count: usize, buf_size: usize, page_size: HugePageSize) -> io::Result<Self> {
        let free = Arc::new(SegQueue::new());
        for _ in 0..count {
            free.push(HugeBuf::alloc(buf_size, page_size)?);
        }
        Ok(Self {
            free,
            available: Arc::new(AtomicUsize::new(count)),
            buf_size,
            page_size,
        })
    }

    /// Acquire a buffer from the pool.
    ///
    /// Returns `None` when all buffers are checked out.
    #[must_use]
    pub fn acquire(&self) -> Option<PooledBuf> {
        let buf = self.free.pop()?;
        self.available.fetch_sub(1, Ordering::AcqRel);
        Some(PooledBuf {
            inner: ManuallyDrop::new(buf),
            pool: Arc::clone(&self.free),
            available: Arc::clone(&self.available),
        })
    }

    /// Number of currently available buffers.
    #[must_use]
    pub fn available(&self) -> usize {
        self.available.load(Ordering::Acquire)
    }

    /// Requested buffer size.
    #[must_use]
    pub fn buf_size(&self) -> usize {
        self.buf_size
    }

    /// Requested huge-page size.
    #[must_use]
    pub fn page_size(&self) -> HugePageSize {
        self.page_size
    }
}

/// RAII handle that returns a [`HugeBuf`] to its originating [`BufPool`].
#[derive(Debug)]
pub struct PooledBuf {
    inner: ManuallyDrop<HugeBuf>,
    pool: Arc<SegQueue<HugeBuf>>,
    available: Arc<AtomicUsize>,
}

impl PooledBuf {
    /// Borrow the underlying buffer.
    #[must_use]
    pub fn buf(&self) -> &HugeBuf {
        &self.inner
    }

    /// Mutably borrow the underlying buffer.
    #[must_use]
    pub fn buf_mut(&mut self) -> &mut HugeBuf {
        &mut self.inner
    }

    /// Return the buffer length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return true when the underlying buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Drop for PooledBuf {
    fn drop(&mut self) {
        // Safety: `inner` is taken exactly once during `drop`.
        let buf = unsafe { ManuallyDrop::take(&mut self.inner) };
        self.pool.push(buf);
        self.available.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::{BufPool, HugePageSize};

    #[test]
    fn checked_out_buffers_return_on_drop() {
        let pool = BufPool::new(2, 1, HugePageSize::Mib2).unwrap();

        let first = pool.acquire().unwrap();
        assert_eq!(pool.available(), 1);

        let second = pool.acquire().unwrap();
        assert_eq!(pool.available(), 0);
        assert!(pool.acquire().is_none());

        drop(first);
        assert_eq!(pool.available(), 1);

        drop(second);
        assert_eq!(pool.available(), 2);
    }
}
