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

#[cfg(test)]
mod tests;

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use core::{
    cell::{Cell, UnsafeCell},
    cmp,
    mem::MaybeUninit,
};

#[cfg(loom)]
use loom::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};

#[cfg(not(loom))]
use alloc::sync::Arc;
#[cfg(not(loom))]
use core::sync::atomic::{AtomicUsize, Ordering};

// choose 64 for targets that support it, otherwise 32
#[cfg(target_pointer_width = "64")]
const LAP_SHIFT: u8 = 32;
#[cfg(target_pointer_width = "32")]
const LAP_SHIFT: u8 = 16;

const INDEX_MASK: usize = (1 << LAP_SHIFT) - 1;
const MAX_LEN: usize = INDEX_MASK;

const STATE_EMPTY: usize = 0;
const STATE_AVAILABLE: usize = 1;
const STATE_WRITING: usize = 2;
// reading can be done by multiple readers
// so we use value that is power of 2
// we add this value to the state, which means there is a reader
const STATE_READING: usize = 4;
const READING_MASK: usize = usize::MAX & !(STATE_READING - 1);

// extracts the lap and index
// top 32bits are the lap
// bottom 32bits are the index
const fn unpack_data_index(index: usize) -> (usize, usize) {
    let lap = index >> LAP_SHIFT;
    let index = index & INDEX_MASK;
    (lap, index)
}

const fn pack_data_index(lap: usize, index: usize) -> usize {
    debug_assert!(lap < (1 << LAP_SHIFT));
    debug_assert!(index < (1 << LAP_SHIFT));
    (lap << LAP_SHIFT) | (index & INDEX_MASK)
}

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
    lap: Cell<usize>,
}

impl<T> Default for Node<T> {
    fn default() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(STATE_EMPTY),
            lap: Cell::new(0),
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
}

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
        }
    }

    fn push(&self, value: T) {
        let mut node;
        let mut should_drop;

        let mut current_head = self.head.load(Ordering::Acquire);

        loop {
            let (producer_lap, producer_index) = unpack_data_index(current_head);

            node = &self.buffer[producer_index % self.buffer.len()];

            // acquire the node
            let mut state;
            loop {
                state = node.state.load(Ordering::Acquire);

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
                                Ordering::AcqRel,
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

            let next_index = (producer_index + 1) % self.buffer.len();
            let next_lap = if next_index == 0 {
                producer_lap + 1
            } else {
                producer_lap
            };
            let next_head = pack_data_index(next_lap, next_index);

            match self.head.compare_exchange(
                current_head,
                next_head,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    node.lap.set(producer_lap);
                    break;
                }
                Err(x) => {
                    current_head = x;
                }
            }
            // rollback and try again
            node.state.store(state, Ordering::Release);
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
        // release the node
        node.state.store(STATE_AVAILABLE, Ordering::Release);
    }

    fn pop(&self, reader: &mut ReaderData) -> Option<T> {
        let (producer_lap, producer_index) = unpack_data_index(self.head.load(Ordering::Acquire));
        let mut reader_index = reader.index;
        let reader_lap = reader.lap;

        match reader_lap.cmp(&producer_lap) {
            // the reader is before the writer
            // so there must be something to read
            cmp::Ordering::Less => {
                let lap_diff = producer_lap - reader_lap;
                let head_diff = producer_index as isize - reader_index as isize;

                if (lap_diff > 0 && head_diff > 0) || lap_diff > 1 {
                    // there is an overflow
                    // we need to update the reader index
                    // we will take the latest readable value, the furthest from the writer
                    // and this is the value at [head, producer_lap - 1]
                    let new_index = producer_index % self.buffer.len();
                    reader_index = new_index;

                    reader.lap = producer_lap - 1;
                }
            }
            cmp::Ordering::Equal => {
                if reader_index >= producer_index {
                    return None;
                }
            }
            cmp::Ordering::Greater => {
                unreachable!("The reader is after the writer");
            }
        }
        let mut node = &self.buffer[reader_index % self.buffer.len()];

        // acquire the node
        loop {
            let state = node.state.load(Ordering::Acquire);

            if is_readable(state) {
                let old = node.state.fetch_add(STATE_READING, Ordering::AcqRel);

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

        // if the node contain a different lap number, then the writer
        // has overwritten the data and finished writing before we got the lock
        // retry the whole thing (slow)
        if node.lap.get() != reader.lap {
            node.state.fetch_sub(STATE_READING, Ordering::Release);
            return self.pop(reader);
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

unsafe impl<T: Clone + Send, const N: usize> Send for Sender<T, N> {}
unsafe impl<T: Clone + Send, const N: usize> Sync for Sender<T, N> {}

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

unsafe impl<T: Clone + Send, const N: usize> Send for Receiver<T, N> {}
unsafe impl<T: Clone + Send, const N: usize> Sync for Receiver<T, N> {}

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
