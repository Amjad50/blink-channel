//! Fast, Bounded, Lossy broadcast channel with `no_std` support.
//! This is implemented with ring buffer and atomic operations, it may be considered lock-free,
//! as we don't use `Lock` premitive, but the implementation may spin waiting for a contending
//! writer/reader to finish accessing a specific node. Its very rare, but
//! maybe I won't call it `lock-free` in the strict sense.
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
//! # #[cfg(not(loom))]
//! # {
//! use blinkcast::alloc::channel;
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
//! # }
//! ```
//! Multiple senders multiple receivers
//! ```
//! # #[cfg(not(loom))]
//! # {
//! use blinkcast::alloc::channel;
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
//! # }
//! ```

#![cfg_attr(not(test), no_std)]
#![cfg_attr(all(test, feature = "unstable"), feature(test))]

use core::{
    cell::{Cell, UnsafeCell},
    mem::MaybeUninit,
};

#[cfg(not(loom))]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(loom)]
use loom::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
mod tests;

#[cfg(feature = "alloc")]
pub mod alloc;
mod core_impl;
#[cfg(not(loom))]
// Atomics in `loom` don't support `const fn`, so it crashes when compiling
// since, we don't need `static_mem` to be tested with `loom`, we can just
// exclude it if we are building for `loom`
pub mod static_mem;

// choose 64 for targets that support it, otherwise 32
#[cfg(target_pointer_width = "64")]
const LAP_SHIFT: u8 = 32;
#[cfg(target_pointer_width = "32")]
const LAP_SHIFT: u8 = 16;

const INDEX_MASK: usize = (1 << LAP_SHIFT) - 1;
/// The maximum length of the buffer allowed on this platform
/// It will be `2^16 - 1` on 32bit platforms and `2^32 - 1` on 64bit platforms
pub const MAX_LEN: usize = INDEX_MASK;

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

impl<T> Node<T> {
    #[cfg(not(loom))]
    // Atomics in `loom` don't support `const fn`, so it crashes when compiling
    // since, we don't need `static_mem` to be tested with `loom`, we can just
    // exclude it if we are building for `loom`
    pub const fn empty() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(STATE_EMPTY),
            lap: Cell::new(0),
        }
    }
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
