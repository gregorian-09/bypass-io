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
use crate::buf::PooledBuf;

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

            let mut total = 0usize;
            for buf in bufs {
                // Safety: each read completes before moving to the next buffer.
                let slice = unsafe { buf.buf_mut().as_slice_mut() };
                let n = self.read_at(fd, slice, offset + total as u64)?;
                total += n;
                if n < slice.len() {
                    break;
                }
            }
            Ok(total)
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

            let mut total = 0usize;
            for buf in bufs {
                let slice = buf.buf().as_slice();
                let n = self.write_at(fd, slice, offset + total as u64)?;
                total += n;
                if n < slice.len() {
                    break;
                }
            }
            Ok(total)
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

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::UringBackend;

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
}
