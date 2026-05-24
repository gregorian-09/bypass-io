use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[repr(align(64))]
#[derive(Debug)]
struct CachePadded<T>(T);

impl<T> CachePadded<T> {
    const fn new(value: T) -> Self {
        Self(value)
    }
}

#[derive(Debug)]
struct Slot<T> {
    ready: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    fn uninit() -> Self {
        Self {
            ready: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

/// Bounded multi-producer single-consumer ring.
///
/// Producers reserve slots with a compare-and-exchange on `head`. The single
/// consumer owns `tail` and drains initialized slots in FIFO order.
#[derive(Debug)]
pub struct MpscRing<T> {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    slots: Box<[Slot<T>]>,
    mask: usize,
}

unsafe impl<T: Send> Send for MpscRing<T> {}
unsafe impl<T: Send> Sync for MpscRing<T> {}

impl<T> MpscRing<T> {
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

        let slots = (0..capacity)
            .map(|_| Slot::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            slots,
            mask: capacity - 1,
        }
    }

    /// Try to push `item` into the ring.
    ///
    /// Returns the item back to the caller when the ring is full or another
    /// producer wins enough reservation races that the ring becomes full.
    pub fn push(&self, item: T) -> Result<(), T> {
        let mut head = self.head.0.load(Ordering::Relaxed);
        loop {
            let tail = self.tail.0.load(Ordering::Acquire);
            if head.wrapping_sub(tail) == self.capacity() {
                return Err(item);
            }

            match self.head.0.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(reserved) => {
                    let slot = &self.slots[reserved & self.mask];
                    // Safety: this producer uniquely reserved `reserved`, and
                    // the capacity check ensures the consumer has released this
                    // slot from any previous lap around the ring.
                    unsafe {
                        (*slot.value.get()).write(item);
                    }
                    slot.ready.store(true, Ordering::Release);
                    return Ok(());
                }
                Err(actual) => head = actual,
            }
        }
    }

    /// Try to pop the next item.
    ///
    /// Returns `None` when the ring is empty or when a producer has reserved
    /// the next FIFO slot but has not finished writing it yet.
    #[must_use]
    pub fn pop(&self) -> Option<T> {
        let tail = self.tail.0.load(Ordering::Relaxed);
        let head = self.head.0.load(Ordering::Acquire);
        if tail == head {
            return None;
        }

        let slot = &self.slots[tail & self.mask];
        if !slot.ready.load(Ordering::Acquire) {
            return None;
        }

        // Safety: the ready flag was observed with acquire ordering after the
        // producer wrote the item and stored `ready = true` with release.
        let item = unsafe { (*slot.value.get()).assume_init_read() };
        slot.ready.store(false, Ordering::Release);
        self.tail.0.store(tail.wrapping_add(1), Ordering::Release);
        Some(item)
    }

    /// Number of slots in the ring.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Approximate number of reserved slots.
    ///
    /// A reserved slot may not yet be ready if a producer is between successful
    /// reservation and publishing the item.
    #[must_use]
    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Return true when no slot is reserved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for MpscRing<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::MpscRing;

    #[test]
    fn push_pop_round_trip() {
        let ring = MpscRing::new(4);
        assert_eq!(ring.push(10), Ok(()));
        assert_eq!(ring.push(20), Ok(()));
        assert_eq!(ring.pop(), Some(10));
        assert_eq!(ring.pop(), Some(20));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn reports_full_without_overwriting_unread_slots() {
        let ring = MpscRing::new(2);
        assert_eq!(ring.push(1), Ok(()));
        assert_eq!(ring.push(2), Ok(()));
        assert_eq!(ring.push(3), Err(3));
        assert_eq!(ring.pop(), Some(1));
        assert_eq!(ring.push(3), Ok(()));
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), Some(3));
    }

    #[test]
    fn accepts_multiple_producers() {
        let ring = Arc::new(MpscRing::new(128));
        let mut handles = Vec::new();

        for producer in 0..4 {
            let ring = Arc::clone(&ring);
            handles.push(thread::spawn(move || {
                for item in 0..16 {
                    let mut value = producer * 100 + item;
                    loop {
                        match ring.push(value) {
                            Ok(()) => break,
                            Err(returned) => {
                                value = returned;
                                thread::yield_now();
                            }
                        }
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let mut values = Vec::new();
        while let Some(value) = ring.pop() {
            values.push(value);
        }
        values.sort_unstable();

        let mut expected = Vec::new();
        for producer in 0..4 {
            for item in 0..16 {
                expected.push(producer * 100 + item);
            }
        }
        expected.sort_unstable();

        assert_eq!(values, expected);
    }
}
