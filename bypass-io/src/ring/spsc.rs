use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

#[repr(align(64))]
#[derive(Debug)]
struct CachePadded<T>(T);

impl<T> CachePadded<T> {
    const fn new(value: T) -> Self {
        Self(value)
    }
}

/// Bounded single-producer single-consumer ring.
///
/// The ring uses one producer-owned index and one consumer-owned index. It
/// requires exactly one producer thread calling [`push`](Self::push) and one
/// consumer thread calling [`pop`](Self::pop).
#[derive(Debug)]
pub struct SpscRing<T> {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    buf: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
}

unsafe impl<T: Send> Send for SpscRing<T> {}
unsafe impl<T: Send> Sync for SpscRing<T> {}

impl<T> SpscRing<T> {
    /// Create a ring with power-of-two `capacity`.
    ///
    /// # Panics
    ///
    /// Panics when `capacity` is not a non-zero power of two.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity.is_power_of_two(),
            "capacity must be a power of two"
        );
        assert!(capacity > 0, "capacity must be non-zero");
        let buf = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            buf,
            mask: capacity - 1,
        }
    }

    /// Try to push `item` into the ring.
    ///
    /// Returns the item back to the caller when the ring is full.
    pub fn push(&self, item: T) -> Result<(), T> {
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);
        if head.wrapping_sub(tail) == self.capacity() {
            return Err(item);
        }

        let index = head & self.mask;
        // Safety: the single producer is the only writer for this slot before
        // publishing the incremented head, and the capacity check guarantees the
        // consumer has already advanced past the slot.
        unsafe {
            (*self.buf[index].get()).write(item);
        }
        self.head.0.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Try to pop the next item from the ring.
    #[must_use]
    pub fn pop(&self) -> Option<T> {
        let tail = self.tail.0.load(Ordering::Relaxed);
        let head = self.head.0.load(Ordering::Acquire);
        if tail == head {
            return None;
        }

        let index = tail & self.mask;
        // Safety: the acquire load of `head` observed the producer's release
        // store, so this slot is initialized and not concurrently written.
        let item = unsafe { (*self.buf[index].get()).assume_init_read() };
        self.tail.0.store(tail.wrapping_add(1), Ordering::Release);
        Some(item)
    }

    /// Number of slots in the ring.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Current approximate number of initialized elements.
    #[must_use]
    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Return true when the ring contains no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for SpscRing<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::SpscRing;

    #[test]
    fn push_pop_round_trip() {
        let ring = SpscRing::new(4);
        assert!(ring.is_empty());
        assert_eq!(ring.push(10), Ok(()));
        assert_eq!(ring.push(20), Ok(()));
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.pop(), Some(10));
        assert_eq!(ring.pop(), Some(20));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn reports_full_without_losing_item() {
        let ring = SpscRing::new(2);
        assert_eq!(ring.push(1), Ok(()));
        assert_eq!(ring.push(2), Ok(()));
        assert_eq!(ring.push(3), Err(3));
        assert_eq!(ring.pop(), Some(1));
        assert_eq!(ring.push(3), Ok(()));
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), Some(3));
    }
}
