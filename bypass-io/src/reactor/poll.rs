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
            .spawn(move || self.run())?;
        Ok(ReactorHandle { stop, handle })
    }

    fn run(self) {
        if let Err(err) = set_cpu_affinity(self.cpu) {
            eprintln!(
                "bypass-io: failed to set reactor affinity to cpu {}: {err}",
                self.cpu
            );
            return;
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
