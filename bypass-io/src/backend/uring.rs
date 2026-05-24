//! `io_uring` backend.
//!
//! This first backend implementation uses the `io-uring` crate directly and
//! completes each operation with `submit_and_wait`. That keeps the borrowed
//! buffer API sound while the project still lacks a cancellation-safe future
//! driver.

use std::fmt;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use io_uring::{opcode, squeue, types, IoUring};

use crate::backend::{BoxIoFuture, DeviceTarget, IoBackend};
use crate::buf::{IoVec, IoVecMut, PooledBuf};

/// Phase 1 `io_uring` backend.
pub struct UringBackend {
    ring: Mutex<IoUring>,
    next_token: AtomicU64,
}

impl fmt::Debug for UringBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UringBackend")
            .field("ring", &"<io_uring>")
            .field("next_token", &self.next_token.load(Ordering::Relaxed))
            .finish()
    }
}

impl UringBackend {
    /// Create a backend with `entries` submission queue entries.
    ///
    /// # Errors
    ///
    /// Returns the OS error reported by `io_uring_setup(2)` when the kernel or
    /// sandbox does not allow creating a ring.
    pub fn new(entries: u32) -> io::Result<Self> {
        Ok(Self {
            ring: Mutex::new(IoUring::new(entries)?),
            next_token: AtomicU64::new(1),
        })
    }

    /// Read into `buf` from file descriptor `fd` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns an OS error if submission fails, the completion result is
    /// negative, or the backend mutex is poisoned.
    pub fn read_at(&self, fd: RawFd, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let token = self.next_token();
        let entry = opcode::Read::new(types::Fd(fd), buf.as_mut_ptr(), buf.len() as _)
            .offset(offset)
            .build()
            .user_data(token);
        self.submit_and_wait(entry, token)
    }

    /// Write `buf` to file descriptor `fd` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns an OS error if submission fails, the completion result is
    /// negative, or the backend mutex is poisoned.
    pub fn write_at(&self, fd: RawFd, buf: &[u8], offset: u64) -> io::Result<usize> {
        let token = self.next_token();
        let entry = opcode::Write::new(types::Fd(fd), buf.as_ptr(), buf.len() as _)
            .offset(offset)
            .build()
            .user_data(token);
        self.submit_and_wait(entry, token)
    }

    /// Read into several buffers from file descriptor `fd` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns an OS error if submission fails, the completion result is
    /// negative, the vector count is too large, or the backend mutex is
    /// poisoned.
    pub fn readv_at(&self, fd: RawFd, bufs: &[IoVecMut<'_>], offset: u64) -> io::Result<usize> {
        let iovecs = libc_iovecs_mut(bufs)?;
        if iovecs.is_empty() {
            return Ok(0);
        }

        let token = self.next_token();
        let entry = opcode::Readv::new(types::Fd(fd), iovecs.as_ptr(), iovecs.len() as u32)
            .offset(offset)
            .build()
            .user_data(token);
        self.submit_and_wait(entry, token)
    }

    /// Write several buffers to file descriptor `fd` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns an OS error if submission fails, the completion result is
    /// negative, the vector count is too large, or the backend mutex is
    /// poisoned.
    pub fn writev_at(&self, fd: RawFd, bufs: &[IoVec<'_>], offset: u64) -> io::Result<usize> {
        let iovecs = libc_iovecs(bufs)?;
        if iovecs.is_empty() {
            return Ok(0);
        }

        let token = self.next_token();
        let entry = opcode::Writev::new(types::Fd(fd), iovecs.as_ptr(), iovecs.len() as u32)
            .offset(offset)
            .build()
            .user_data(token);
        self.submit_and_wait(entry, token)
    }

    /// Flush file descriptor `fd` with `IORING_OP_FSYNC`.
    ///
    /// # Errors
    ///
    /// Returns an OS error if submission fails, the completion result is
    /// negative, or the backend mutex is poisoned.
    pub fn fsync(&self, fd: RawFd) -> io::Result<()> {
        let token = self.next_token();
        let entry = opcode::Fsync::new(types::Fd(fd)).build().user_data(token);
        self.submit_and_wait(entry, token).map(|_| ())
    }

    fn next_token(&self) -> u64 {
        self.next_token.fetch_add(1, Ordering::Relaxed)
    }

    fn submit_and_wait(&self, entry: squeue::Entry, token: u64) -> io::Result<usize> {
        let mut ring = self
            .ring
            .lock()
            .map_err(|_| io::Error::other("io_uring mutex poisoned"))?;

        // Safety: the entry's file descriptor and buffer pointer are supplied by
        // the caller and remain valid until `submit_and_wait` returns. This
        // backend does not return `Pending`, so the borrowed buffer cannot be
        // dropped while the kernel owns the operation.
        unsafe {
            ring.submission()
                .push(&entry)
                .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "submission queue full"))?;
        }

        ring.submit_and_wait(1)?;

        let completion = ring
            .completion()
            .find(|cqe| cqe.user_data() == token)
            .ok_or_else(|| io::Error::other("completion queue did not contain submitted token"))?;

        let result = completion.result();
        if result < 0 {
            Err(io::Error::from_raw_os_error(-result))
        } else {
            Ok(result as usize)
        }
    }
}

impl IoBackend for UringBackend {
    type Error = io::Error;

    fn read<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a mut PooledBuf,
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let DeviceTarget::Fd(fd) = target else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "UringBackend requires DeviceTarget::Fd",
                ));
            };
            // Safety: this backend completes the read before the future returns
            // `Ready`, so mutable access does not outlive the operation.
            let slice = unsafe { buf.buf_mut().as_slice_mut() };
            self.read_at(fd, slice, offset)
        })
    }

    fn write<'a>(
        &'a self,
        target: DeviceTarget,
        buf: &'a PooledBuf,
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let DeviceTarget::Fd(fd) = target else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "UringBackend requires DeviceTarget::Fd",
                ));
            };
            self.write_at(fd, buf.buf().as_slice(), offset)
        })
    }

    fn readv<'a>(
        &'a self,
        target: DeviceTarget,
        bufs: &'a mut [PooledBuf],
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let DeviceTarget::Fd(fd) = target else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "UringBackend requires DeviceTarget::Fd",
                ));
            };

            let iovecs = bufs
                .iter_mut()
                // Safety: `readv_at` completes before this future returns
                // `Ready`, so the mutable vectors do not outlive the operation.
                .map(|buf| unsafe { IoVecMut::from_pooled_buf(buf) })
                .collect::<Vec<_>>();
            self.readv_at(fd, &iovecs, offset)
        })
    }

    fn writev<'a>(
        &'a self,
        target: DeviceTarget,
        bufs: &'a [PooledBuf],
        offset: u64,
    ) -> BoxIoFuture<'a, usize, Self::Error> {
        Box::pin(async move {
            let DeviceTarget::Fd(fd) = target else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "UringBackend requires DeviceTarget::Fd",
                ));
            };

            let iovecs = bufs.iter().map(IoVec::from_pooled_buf).collect::<Vec<_>>();
            self.writev_at(fd, &iovecs, offset)
        })
    }

    fn flush<'a>(&'a self, target: DeviceTarget) -> BoxIoFuture<'a, (), Self::Error> {
        Box::pin(async move {
            let DeviceTarget::Fd(fd) = target else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "UringBackend requires DeviceTarget::Fd",
                ));
            };
            self.fsync(fd)
        })
    }

    fn poll_completions(&self) -> usize {
        0
    }
}

fn libc_iovecs(bufs: &[IoVec<'_>]) -> io::Result<Vec<libc::iovec>> {
    if bufs.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many iovecs for io_uring",
        ));
    }

    Ok(bufs
        .iter()
        .map(|buf| {
            let raw = buf.as_raw();
            libc::iovec {
                iov_base: raw.iov_base,
                iov_len: raw.iov_len,
            }
        })
        .collect())
}

fn libc_iovecs_mut(bufs: &[IoVecMut<'_>]) -> io::Result<Vec<libc::iovec>> {
    if bufs.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many iovecs for io_uring",
        ));
    }

    Ok(bufs
        .iter()
        .map(|buf| {
            let raw = buf.as_raw();
            libc::iovec {
                iov_base: raw.iov_base,
                iov_len: raw.iov_len,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::UringBackend;
    use crate::buf::{IoVec, IoVecMut};

    static NEXT_TEST_FILE: AtomicUsize = AtomicUsize::new(0);

    fn temp_file_path(name: &str) -> PathBuf {
        let unique = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("bypass-io-{name}-{}-{unique}", process::id()))
    }

    fn backend_or_skip() -> Option<UringBackend> {
        match UringBackend::new(8) {
            Ok(backend) => Some(backend),
            Err(err) => {
                eprintln!("skipping io_uring test: {err}");
                None
            }
        }
    }

    #[test]
    fn write_and_read_file_offsets() -> std::io::Result<()> {
        let Some(backend) = backend_or_skip() else {
            return Ok(());
        };

        let path = temp_file_path("rw");
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)?;
        let fd = file.as_raw_fd();

        let written = backend.write_at(fd, b"hello", 4)?;
        assert_eq!(written, 5);
        backend.fsync(fd)?;

        let mut read_buf = [0u8; 5];
        let read = backend.read_at(fd, &mut read_buf, 4)?;
        assert_eq!(read, 5);
        assert_eq!(&read_buf, b"hello");

        file.seek(SeekFrom::Start(0))?;
        let mut all = Vec::new();
        file.read_to_end(&mut all)?;
        assert_eq!(&all[4..9], b"hello");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn write_at_matches_std_file_contents() -> std::io::Result<()> {
        let Some(backend) = backend_or_skip() else {
            return Ok(());
        };

        let path = temp_file_path("write");
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)?;
        file.write_all(b"--------")?;

        let fd = file.as_raw_fd();
        assert_eq!(backend.write_at(fd, b"IO", 2)?, 2);

        file.seek(SeekFrom::Start(0))?;
        let mut all = Vec::new();
        file.read_to_end(&mut all)?;
        assert_eq!(&all, b"--IO----");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn writev_and_readv_use_single_vectored_operations() -> std::io::Result<()> {
        let Some(backend) = backend_or_skip() else {
            return Ok(());
        };

        let path = temp_file_path("rwv");
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)?;
        let fd = file.as_raw_fd();

        let left = b"hello ";
        let right = b"uring";
        let write_iovecs = [IoVec::from_slice(left), IoVec::from_slice(right)];
        assert_eq!(
            backend.writev_at(fd, &write_iovecs, 3)?,
            left.len() + right.len()
        );

        let mut read_left = [0u8; 6];
        let mut read_right = [0u8; 5];
        let read_iovecs = [
            IoVecMut::from_mut_slice(&mut read_left),
            IoVecMut::from_mut_slice(&mut read_right),
        ];
        assert_eq!(
            backend.readv_at(fd, &read_iovecs, 3)?,
            read_left.len() + read_right.len()
        );

        assert_eq!(&read_left, left);
        assert_eq!(&read_right, right);

        fs::remove_file(path)?;
        Ok(())
    }
}
