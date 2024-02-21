//! Fast, Lock-free, Bounded, Lossy `no_std` broadcast channel.
//! This is implemented with ring buffer and atomic operations, which provide us with lock-free behavior with
//! no extra dependencies.
//!
//! The API of the `blinkcast` is similar to that of the [`std::sync::mpsc`](https://doc.rust-lang.org/std/sync/mpsc/) channels.
//! However, there are some differences:
//!
//! - It allows for multiple consumers (receivers) and multiple prodocuers (senders).
//! - The channel broadcasts every send to every consumer.
//! - Lossy, the sender will overwrite old data, so receivers must be quick or they will lose the old data (don'
//! t blink).
//! - Implemented for `no_std` environments.
//!
//! The data sent must implment `Clone`, because it will be kept in the buffer, and readers can read it multiple times.
//!
//! The original object will remain in the buffer until its overwritten, at that point it will be dropped.
//! Thus be careful if the value is a large allocation for example big `Arc`. One of the clones (original) will
//! be kept by the buffer and will result in a delayed deallocation if that was not expected by the user.
//! See [issue #1](https://github.com/Amjad50/blinkcast/issues/1)
//!
//! # Example
//! Single sender multiple receivers
//! ```
//! # #[cfg(loom)]
//! # loom::model(|| {
//! use blinkcast::channel;
//!
//! let (sender, mut receiver1) = channel::<i32, 4>();
//! sender.send(1);
//! sender.send(2);
//!
//! let mut receiver2 = receiver1.clone();
//!
//! assert_eq!(receiver1.recv(), Some(1));
//! assert_eq!(receiver1.recv(), Some(2));
//! assert_eq!(receiver1.recv(), None);
//!
//! assert_eq!(receiver2.recv(), Some(1));
//! assert_eq!(receiver2.recv(), Some(2));
//! assert_eq!(receiver2.recv(), None);
//! # });
//! ```
//! Multiple senders multiple receivers
//! ```
//! # #[cfg(loom)]
//! # loom::model(|| {
//! use blinkcast::channel;
//! use std::thread;
//! let (sender1, mut receiver1) = channel::<i32, 100>();
//! let sender2 = sender1.clone();
//!
//! let t1 = thread::spawn(move || {
//!     for i in 0..50 {
//!         sender1.send(i);
//!     }
//! });
//! let t2 = thread::spawn(move || {
//!     for i in 0..50 {
//!         sender2.send(i);
//!     }
//! });
//!
//! t1.join().unwrap();
//! t2.join().unwrap();
//!
//! let mut receiver2 = receiver1.clone();
//!
//! let mut sum1 = 0;
//! let mut sum2 = 0;
//! for i in 0..100 {
//!    let v1 = receiver1.recv().unwrap();
//!    let v2 = receiver2.recv().unwrap();
//!    sum1 += v1;
//!    sum2 += v2;
//!    assert_eq!(v1, v2);
//! }
//! assert_eq!(sum1, 49 * 50);
//! assert_eq!(sum2, 49 * 50);
//! # });
//! ```

#![cfg_attr(not(test), no_std)]
#![cfg_attr(all(test, feature = "unstable"), feature(test))]

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use core::{cell::UnsafeCell, cmp, mem::MaybeUninit};

#[cfg(loom)]
use loom::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[cfg(not(loom))]
use alloc::sync::Arc;
#[cfg(not(loom))]
use core::sync::atomic::{AtomicUsize, Ordering};

const STATE_EMPTY: usize = 0;
const STATE_AVAILABLE: usize = 1;
const STATE_WRITING: usize = 2;
// reading can be done by multiple readers
// so we use value that is power of 2
// we add this value to the state, which means there is a reader
const STATE_READING: usize = 4;
const READING_MASK: usize = usize::MAX & !(STATE_READING - 1);

#[inline]
fn is_reading(state: usize) -> bool {
    state & READING_MASK != 0
}

#[inline]
fn is_readable(state: usize) -> bool {
    state == STATE_AVAILABLE || is_reading(state)
}

struct Node<T> {
    data: UnsafeCell<MaybeUninit<T>>,
    state: AtomicUsize,
}

impl<T> Default for Node<T> {
    fn default() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(STATE_EMPTY),
        }
    }
}

#[derive(Clone)]
struct ReaderData {
    index: usize,
    lap: usize,
}

struct InnerChannel<T, const N: usize> {
    buffer: Box<[Node<T>]>,
    head: AtomicUsize,
    producer_lap: AtomicUsize,
}

unsafe impl<T: Clone + Send, const N: usize> Send for InnerChannel<T, N> {}
unsafe impl<T: Clone + Send, const N: usize> Sync for InnerChannel<T, N> {}

impl<T: Clone, const N: usize> InnerChannel<T, N> {
    fn new() -> Self {
        let mut buffer = Vec::with_capacity(N);
        for _ in 0..N {
            buffer.push(Default::default());
        }
        let buffer = buffer.into_boxed_slice();
        Self {
            buffer,
            head: AtomicUsize::new(0),
            producer_lap: AtomicUsize::new(0),
        }
    }

    fn push(&self, value: T) {
        let mut current_head;
        let mut next_head;

        // reserves a slot for writing not shared with other writers
        loop {
            current_head = self.head.load(Ordering::Acquire);
            next_head = (current_head + 1) % self.buffer.len();

            if self
                .head
                .compare_exchange_weak(
                    current_head,
                    next_head,
                    Ordering::Release,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        if next_head == 0 {
            self.producer_lap.fetch_add(1, Ordering::Release);
        }

        let node = &self.buffer[current_head % self.buffer.len()];

        let should_drop;
        loop {
            let mut state = node.state.load(Ordering::Acquire);

            while is_reading(state) {
                core::hint::spin_loop();
                // wait until the reader is done
                state = node.state.load(Ordering::Acquire);
            }

            match state {
                STATE_EMPTY | STATE_AVAILABLE => {
                    if node
                        .state
                        .compare_exchange_weak(
                            state,
                            STATE_WRITING,
                            Ordering::Release,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        should_drop = state == STATE_AVAILABLE;
                        break;
                    }
                }
                STATE_WRITING => unreachable!("There should be no writer writing"),
                s => unreachable!("Invalid state: {}", s),
            }
        }

        if should_drop {
            // Safety: we have exclusive access to the node
            unsafe {
                node.data.get().read().assume_init_drop();
            }
        }

        // Safety: we have exclusive access to the node
        unsafe {
            node.data.get().write(MaybeUninit::new(value));
        }

        // publish the value
        node.state.store(STATE_AVAILABLE, Ordering::Release);
    }

    fn pop(&self, reader: &mut ReaderData) -> Option<T> {
        let current_head = self.head.load(Ordering::Acquire);
        let mut reader_index = reader.index;
        let reader_lap = reader.lap;

        let producer_lap = self.producer_lap.load(Ordering::Relaxed);
        match reader_lap.cmp(&producer_lap) {
            // the reader is before the writer
            // so there must be something to read
            cmp::Ordering::Less => {
                let lap_diff = producer_lap - reader_lap;
                let head_diff = current_head as isize - reader_index as isize;

                if (lap_diff > 0 && head_diff > 0) || lap_diff > 1 {
                    // there is an overflow
                    // we need to update the reader index
                    // we will take the latest readable value, the furthest from the writer
                    // and this is the value at [head, producer_lap - 1]
                    let new_index = current_head % self.buffer.len();
                    reader_index = new_index;

                    reader.lap = producer_lap - 1;
                }
            }
            cmp::Ordering::Equal => {
                if reader_index >= current_head {
                    return None;
                }
            }
            cmp::Ordering::Greater => {
                unreachable!("The reader is after the writer");
            }
        }

        let mut node = &self.buffer[reader_index % self.buffer.len()];

        loop {
            let state = node.state.load(Ordering::Acquire);

            if is_readable(state) {
                let old = node.state.fetch_add(STATE_READING, Ordering::Release);

                if is_readable(old) {
                    break;
                }
                // something happened, rollback
                node.state.fetch_sub(STATE_READING, Ordering::Release);
                continue;
            }

            match state {
                STATE_WRITING => {
                    reader_index += 1;
                    node = &self.buffer[reader_index % self.buffer.len()];
                }
                STATE_EMPTY => unreachable!("There should be some data at least"),
                s => unreachable!("Invalid state: {}", s),
            }
        }

        let data = unsafe { node.data.get().read().assume_init_ref().clone() };

        reader.index = (reader_index + 1) % self.buffer.len();
        if reader.index == 0 {
            reader.lap += 1;
        }

        node.state.fetch_sub(STATE_READING, Ordering::Release);

        Some(data)
    }
}

/// The sender of the [`channel`].
///
/// This is a cloneable sender, so you can have multiple senders that will send to the same
/// channel.
///
/// Broadcast messages sent by using the [`send`](Sender::send) method.
///
/// # Examples
/// ```
/// # #[cfg(loom)]
/// # loom::model(|| {
/// use blinkcast::channel;
///
/// let (sender, mut receiver) = channel::<i32, 4>();
///
/// sender.send(1);
/// let sender2 = sender.clone();
/// sender2.send(2);
///
/// assert_eq!(receiver.recv(), Some(1));
/// assert_eq!(receiver.recv(), Some(2));
/// assert_eq!(receiver.recv(), None);
/// # });
pub struct Sender<T, const N: usize> {
    queue: Arc<InnerChannel<T, N>>,
}

impl<T: Clone, const N: usize> Sender<T, N> {
    /// Sends a message to the channel.
    /// If the channel is full, the oldest message will be overwritten.
    /// So the receiver must be quick or it will lose the old data.
    pub fn send(&self, value: T) {
        self.queue.push(value);
    }
}

impl<T, const N: usize> Clone for Sender<T, N> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
        }
    }
}

/// The receiver of the [`channel`].
///
/// This is a cloneable receiver, so you can have multiple receivers that start from the same
/// point.
///
/// Broadcast messages sent by the channel are received by the [`recv`](Receiver::recv) method.
///
/// # Examples
/// ```
/// # #[cfg(loom)]
/// # loom::model(|| {
/// use blinkcast::channel;
/// let (sender, mut receiver) = channel::<i32, 4>();
/// sender.send(1);
/// assert_eq!(receiver.recv(), Some(1));
///
/// sender.send(2);
/// sender.send(3);
///
/// assert_eq!(receiver.recv(), Some(2));
///
/// // clone the receiver
/// let mut receiver2 = receiver.clone();
/// assert_eq!(receiver.recv(), Some(3));
/// assert_eq!(receiver2.recv(), Some(3));
/// assert_eq!(receiver.recv(), None);
/// assert_eq!(receiver2.recv(), None);
/// # });
/// ```
pub struct Receiver<T, const N: usize> {
    queue: Arc<InnerChannel<T, N>>,
    reader: ReaderData,
}

impl<T: Clone, const N: usize> Receiver<T, N> {
    /// Receives a message from the channel.
    ///
    /// If there is no message available, this method will return `None`.
    pub fn recv(&mut self) -> Option<T> {
        self.queue.pop(&mut self.reader)
    }
}

impl<T: Clone, const N: usize> Clone for Receiver<T, N> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            reader: self.reader.clone(),
        }
    }
}

/// Creates a new channel, returning the [`Sender`] and [`Receiver`] for it.
///
/// Both of the sender and receiver are cloneable, so you can have multiple senders and receivers.
///
/// # Examples
/// ```
/// # #[cfg(loom)]
/// # loom::model(|| {
/// use blinkcast::channel;
/// let (sender, mut receiver) = channel::<i32, 4>();
///
/// sender.send(1);
/// sender.send(2);
///
/// assert_eq!(receiver.recv(), Some(1));
///
/// let sender2 = sender.clone();
/// sender2.send(3);
///
/// assert_eq!(receiver.recv(), Some(2));
///
/// let mut receiver2 = receiver.clone();
/// assert_eq!(receiver.recv(), Some(3));
/// assert_eq!(receiver2.recv(), Some(3));
/// assert_eq!(receiver.recv(), None);
/// assert_eq!(receiver2.recv(), None);
/// # });
/// ```
pub fn channel<T: Clone, const N: usize>() -> (Sender<T, N>, Receiver<T, N>) {
    let queue = Arc::new(InnerChannel::<T, N>::new());
    (
        Sender {
            queue: queue.clone(),
        },
        Receiver {
            queue,
            reader: ReaderData { index: 0, lap: 0 },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! loom {
        ($b:block) => {
            #[cfg(loom)]
            {
                loom::model(|| $b)
            }
            #[cfg(not(loom))]
            {
                $b
            }
        };
    }

    #[test]
    fn test_push_pop() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 4>();

            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);

            assert_eq!(receiver.recv(), Some(1));
            assert_eq!(receiver.recv(), Some(2));
            assert_eq!(receiver.recv(), Some(3));
            assert_eq!(receiver.recv(), Some(4));
            assert_eq!(receiver.recv(), None);
        });
    }

    #[test]
    fn test_more_push_pop() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 4>();

            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);

            assert_eq!(receiver.recv(), Some(1));
            assert_eq!(receiver.recv(), Some(2));

            sender.send(5);
            sender.send(6);

            assert_eq!(receiver.recv(), Some(3));
            assert_eq!(receiver.recv(), Some(4));
            assert_eq!(receiver.recv(), Some(5));
            assert_eq!(receiver.recv(), Some(6));
            assert_eq!(receiver.recv(), None);
        });
    }

    #[test]
    fn test_clone_send() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 6>();

            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);

            let sender2 = sender.clone();

            sender2.send(5);
            sender2.send(6);

            assert_eq!(receiver.recv(), Some(1));
            assert_eq!(receiver.recv(), Some(2));
            assert_eq!(receiver.recv(), Some(3));
            assert_eq!(receiver.recv(), Some(4));
            assert_eq!(receiver.recv(), Some(5));
            assert_eq!(receiver.recv(), Some(6));
            assert_eq!(receiver.recv(), None);
        });
    }

    #[test]
    fn test_clone_recv() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 4>();

            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);

            let mut receiver2 = receiver.clone();

            assert_eq!(receiver.recv(), Some(1));
            assert_eq!(receiver2.recv(), Some(1));
            assert_eq!(receiver.recv(), Some(2));
            assert_eq!(receiver2.recv(), Some(2));
            assert_eq!(receiver.recv(), Some(3));
            assert_eq!(receiver2.recv(), Some(3));
            assert_eq!(receiver.recv(), Some(4));
            assert_eq!(receiver2.recv(), Some(4));
            assert_eq!(receiver.recv(), None);
            assert_eq!(receiver2.recv(), None);
        });
    }

    #[test]
    fn test_middle_clone() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 4>();

            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);

            assert_eq!(receiver.recv(), Some(1));
            assert_eq!(receiver.recv(), Some(2));

            let mut receiver2 = receiver.clone();

            sender.send(5);
            sender.send(6);

            assert_eq!(receiver.recv(), Some(3));
            assert_eq!(receiver2.recv(), Some(3));
            assert_eq!(receiver.recv(), Some(4));
            assert_eq!(receiver2.recv(), Some(4));
            assert_eq!(receiver.recv(), Some(5));
            assert_eq!(receiver2.recv(), Some(5));
            assert_eq!(receiver.recv(), Some(6));
            assert_eq!(receiver2.recv(), Some(6));
            assert_eq!(receiver.recv(), None);
            assert_eq!(receiver2.recv(), None);
        });
    }

    #[test]
    fn test_overflow() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 4>();

            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);
            sender.send(5);
            sender.send(6);
            sender.send(7);
            sender.send(8);

            assert_eq!(receiver.recv(), Some(5));
            assert_eq!(receiver.recv(), Some(6));
            assert_eq!(receiver.recv(), Some(7));

            sender.send(9);
            sender.send(10);
            sender.send(11);
            sender.send(12);

            assert_eq!(receiver.recv(), Some(9));
            assert_eq!(receiver.recv(), Some(10));
            assert_eq!(receiver.recv(), Some(11));
            assert_eq!(receiver.recv(), Some(12));
            assert_eq!(receiver.recv(), None);
        });
    }

    #[test]
    fn test_always_overflow() {
        loom!({
            let (sender, mut receiver) = channel::<i32, 4>();

            for i in 0..100 {
                sender.send(i);
            }

            for i in 100 - 4..100 {
                assert_eq!(receiver.recv(), Some(i));
            }
            assert_eq!(receiver.recv(), None);
        });
    }

    #[test]
    fn test_drop() {
        loom!({
            #[cfg(not(loom))]
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            #[cfg(loom)]
            // loom doesn't support `const-fn` atomics
            loom::lazy_static! {
                static ref COUNTER: AtomicUsize = AtomicUsize::new(0);
            }

            #[derive(Clone, Eq, PartialEq, Debug)]
            struct DropCount;
            impl Drop for DropCount {
                fn drop(&mut self) {
                    COUNTER.fetch_add(1, Ordering::Relaxed);
                }
            }

            let (sender, mut receiver) = channel::<DropCount, 4>();

            sender.send(DropCount);
            sender.send(DropCount);
            sender.send(DropCount);
            sender.send(DropCount);

            assert_eq!(COUNTER.load(Ordering::Relaxed), 0);

            // overflowing will drop the oldest value
            sender.send(DropCount);
            assert_eq!(COUNTER.load(Ordering::Relaxed), 1);
            sender.send(DropCount);
            assert_eq!(COUNTER.load(Ordering::Relaxed), 2);

            // receiving won't drop the original value, but the clone will be dropped normally when leaving the scope
            // we are taking the values here so that it won't be dropped until we finish this test
            let _v1 = receiver.recv().unwrap();
            let _v2 = receiver.recv().unwrap();
            let _v3 = receiver.recv().unwrap();
            let _v4 = receiver.recv().unwrap();
            assert_eq!(receiver.recv(), None);
            // no change
            assert_eq!(COUNTER.load(Ordering::Relaxed), 2);
        });
    }

    #[test]
    #[cfg(not(loom))]
    fn stress_test() {
        let (sender, receiver) = channel::<i32, 40>();

        for _ in 0..4 {
            for i in 0..10 {
                sender.send(i);
            }
        }

        let mut receivers = Vec::new();
        for _ in 0..4 {
            let mut receiver = receiver.clone();
            receivers.push(std::thread::spawn(move || {
                let mut sum = 0;
                for _ in 0..40 {
                    sum += receiver.recv().unwrap();
                }
                assert_eq!(sum, 45 * 4);
            }));
        }

        for receiver in receivers {
            receiver.join().unwrap();
        }
    }
}

#[cfg(all(test, not(loom), feature = "unstable"))]
mod bench {
    use super::*;

    extern crate test;
    use test::Bencher;

    #[bench]
    fn bench_push_pop(b: &mut Bencher) {
        let (sender, mut receiver) = channel::<i32, 4>();

        b.iter(|| {
            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);

            receiver.recv().unwrap();
            receiver.recv().unwrap();
            receiver.recv().unwrap();
            receiver.recv().unwrap();
        });
    }

    #[bench]
    fn bench_std_mspc(b: &mut Bencher) {
        let (sender, receiver) = std::sync::mpsc::channel();

        b.iter(|| {
            sender.send(1).unwrap();
            sender.send(2).unwrap();
            sender.send(3).unwrap();
            sender.send(4).unwrap();

            receiver.recv().unwrap();
            receiver.recv().unwrap();
            receiver.recv().unwrap();
            receiver.recv().unwrap();
        });
    }

    #[bench]
    fn bench_push_pop_threaded(b: &mut Bencher) {
        let (sender, receiver) = channel::<i32, 4>();

        b.iter(|| {
            let sender = sender.clone();
            let mut receiver = receiver.clone();

            let sender_thread = std::thread::spawn(move || {
                sender.send(1);
                sender.send(2);
                sender.send(3);
                sender.send(4);
            });

            let recv_thread = std::thread::spawn(move || {
                receiver.recv();
                receiver.recv();
                receiver.recv();
                receiver.recv();
            });

            sender_thread.join().unwrap();
            recv_thread.join().unwrap();
        });
    }

    #[bench]
    fn bench_overflow(b: &mut Bencher) {
        let (sender, mut receiver) = channel::<i32, 4>();

        b.iter(|| {
            sender.send(1);
            sender.send(2);
            sender.send(3);
            sender.send(4);
            sender.send(5);
            sender.send(6);
            sender.send(7);
            sender.send(8);

            assert_eq!(receiver.recv(), Some(5));
            assert_eq!(receiver.recv(), Some(6));
            assert_eq!(receiver.recv(), Some(7));

            sender.send(9);
            sender.send(10);
            sender.send(11);
            sender.send(12);

            assert_eq!(receiver.recv(), Some(9));
            assert_eq!(receiver.recv(), Some(10));
            assert_eq!(receiver.recv(), Some(11));
            assert_eq!(receiver.recv(), Some(12));
            assert_eq!(receiver.recv(), None);
        });
    }

    #[bench]
    fn bench_always_overflow(b: &mut Bencher) {
        let (sender, mut receiver) = channel::<i32, 4>();

        b.iter(|| {
            for i in 0..50 {
                sender.send(i);
            }

            for i in 50 - 4..50 {
                assert_eq!(receiver.recv(), Some(i));
            }
            assert_eq!(receiver.recv(), None);
        });
    }
}
