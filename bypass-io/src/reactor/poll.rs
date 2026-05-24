use std::io;
use std::sync::atomic::{compiler_fence, AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::backend::IoBackend;

use super::set_cpu_affinity;

/// Busy-poll reactor pinned to one CPU core.
#[derive(Debug)]
pub struct PollReactor<B: IoBackend> {
    backend: Arc<B>,
    cpu: usize,
    stop: Arc<AtomicBool>,
}

impl<B: IoBackend> PollReactor<B> {
    /// Create a reactor for `backend` on `cpu`.
    #[must_use]
    pub fn new(backend: Arc<B>, cpu: usize) -> Self {
        Self {
            backend,
            cpu,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Spawn the reactor on a dedicated OS thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the thread cannot be spawned.
    pub fn spawn(self) -> io::Result<ReactorHandle> {
        let stop = Arc::clone(&self.stop);
        let handle = thread::Builder::new()
            .name(format!("bypass-reactor-cpu{}", self.cpu))
            .spawn(move || self.run(true))?;
        Ok(ReactorHandle { stop, handle })
    }

    #[cfg(test)]
    fn spawn_unpinned_for_test(self) -> io::Result<ReactorHandle> {
        let stop = Arc::clone(&self.stop);
        let handle = thread::Builder::new()
            .name(format!("bypass-reactor-test-cpu{}", self.cpu))
            .spawn(move || self.run(false))?;
        Ok(ReactorHandle { stop, handle })
    }

    fn run(self, pin_cpu: bool) {
        if pin_cpu {
            if let Err(err) = set_cpu_affinity(self.cpu) {
                eprintln!(
                    "bypass-io: failed to set reactor affinity to cpu {}: {err}",
                    self.cpu
                );
                return;
            }
        }

        while !self.stop.load(Ordering::Relaxed) {
            let completions = self.backend.poll_completions();
            if completions == 0 {
                compiler_fence(Ordering::SeqCst);
                std::hint::spin_loop();
            }
        }
    }
}

/// Handle used to stop and join a reactor thread.
#[derive(Debug)]
pub struct ReactorHandle {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl ReactorHandle {
    /// Signal the reactor to stop and wait for its thread to exit.
    pub fn shutdown(self) -> thread::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join()
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use crate::backend::{BoxIoFuture, DeviceTarget};
    use crate::buf::PooledBuf;

    use super::*;

    #[derive(Debug)]
    struct TestBackend {
        polls: AtomicUsize,
    }

    impl TestBackend {
        fn polls(&self) -> usize {
            self.polls.load(Ordering::Relaxed)
        }
    }

    #[derive(Debug)]
    struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("test backend error")
        }
    }

    impl Error for TestError {}

    impl IoBackend for TestBackend {
        type Error = TestError;

        fn read<'a>(
            &'a self,
            _target: DeviceTarget,
            _buf: &'a mut PooledBuf,
            _offset: u64,
        ) -> BoxIoFuture<'a, usize, Self::Error> {
            Box::pin(async { unreachable!("reactor test does not submit reads") })
        }

        fn write<'a>(
            &'a self,
            _target: DeviceTarget,
            _buf: &'a PooledBuf,
            _offset: u64,
        ) -> BoxIoFuture<'a, usize, Self::Error> {
            Box::pin(async { unreachable!("reactor test does not submit writes") })
        }

        fn readv<'a>(
            &'a self,
            _target: DeviceTarget,
            _bufs: &'a mut [PooledBuf],
            _offset: u64,
        ) -> BoxIoFuture<'a, usize, Self::Error> {
            Box::pin(async { unreachable!("reactor test does not submit readv") })
        }

        fn writev<'a>(
            &'a self,
            _target: DeviceTarget,
            _bufs: &'a [PooledBuf],
            _offset: u64,
        ) -> BoxIoFuture<'a, usize, Self::Error> {
            Box::pin(async { unreachable!("reactor test does not submit writev") })
        }

        fn flush<'a>(&'a self, _target: DeviceTarget) -> BoxIoFuture<'a, (), Self::Error> {
            Box::pin(async { unreachable!("reactor test does not submit flush") })
        }

        fn poll_completions(&self) -> usize {
            self.polls.fetch_add(1, Ordering::Relaxed);
            0
        }
    }

    #[test]
    fn reactor_polls_until_shutdown() {
        let backend = Arc::new(TestBackend {
            polls: AtomicUsize::new(0),
        });
        let reactor = PollReactor::new(Arc::clone(&backend), 0);
        let handle = reactor.spawn_unpinned_for_test().unwrap();

        let deadline = Instant::now() + Duration::from_millis(250);
        while backend.polls() < 100 && Instant::now() < deadline {
            thread::yield_now();
        }

        handle.shutdown().unwrap();
        assert!(backend.polls() >= 100);
    }
}
