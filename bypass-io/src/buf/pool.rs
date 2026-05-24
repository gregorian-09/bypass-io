use std::io;
use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

use super::{HugeBuf, HugePageSize};

/// Fixed-size pool of reusable [`HugeBuf`] values.
#[derive(Debug)]
pub struct BufPool {
    free: Arc<Mutex<Vec<HugeBuf>>>,
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
        let mut free = Vec::with_capacity(count);
        for _ in 0..count {
            free.push(HugeBuf::alloc(buf_size, page_size)?);
        }
        Ok(Self {
            free: Arc::new(Mutex::new(free)),
            buf_size,
            page_size,
        })
    }

    /// Acquire a buffer from the pool.
    ///
    /// Returns `None` when all buffers are checked out.
    #[must_use]
    pub fn acquire(&self) -> Option<PooledBuf> {
        let buf = self.free.lock().ok()?.pop()?;
        Some(PooledBuf {
            inner: ManuallyDrop::new(buf),
            pool: Arc::clone(&self.free),
        })
    }

    /// Number of currently available buffers.
    #[must_use]
    pub fn available(&self) -> usize {
        self.free.lock().map_or(0, |free| free.len())
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
    pool: Arc<Mutex<Vec<HugeBuf>>>,
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
        if let Ok(mut free) = self.pool.lock() {
            free.push(buf);
        }
    }
}
