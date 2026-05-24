//! Busy-poll reactor for backend completion queues.

mod affinity;
mod poll;

pub use affinity::set_cpu_affinity;
pub use poll::{PollReactor, ReactorHandle};
