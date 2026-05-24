//! Bounded rings for hand-off between hot-path components.

mod mpsc;
mod spsc;

pub use mpsc::MpscRing;
pub use spsc::SpscRing;
