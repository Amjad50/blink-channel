//! A channel implemented with heap allocated buffers (require `alloc` feature).
//!
//! This module implements a broadcast channel with a fixed-size buffer, allocated
//! in the heap. This means that multiple senders can be used without the need to use the same reference.
//!
//! The channel overwrites the oldest message if the buffer is full, prioritizing the latest data.
//!
//! **Key Features:**
//!
//! * **Broadcast:** Multiple senders can send messages to multiple receivers simultaneously.
//! * **Heap-allocated Buffers:** Ensures data storage flexibility when the application requires it.
//! * **Fixed-size Buffer:** Provides bounded memory usage with predictable performance.
//! * **Overwriting Behavior:** Prioritizes the latest data in scenarios where the buffer becomes full.
//! * **Cloneable:** Both `Sender` and `Receiver` are cloneable, enabling flexible message distribution patterns.
//!
//! **Usage Considerations:**
//! * Well-suited for scenarios where multiple components need to broadcast messages and the latest data takes priority.
//! * Ideal when heap allocation is necessary or desirable.
//! * Receivers must be fast enough to keep up with the senders and avoid losing messages due to overwriting.
//!
//! # Examples
//! ```
//! # #[cfg(not(loom))]
//! # {
//! use blinkcast::alloc::channel;
//! let (sender, mut receiver) = channel::<i32, 4>();
//! sender.send(1);
//! assert_eq!(receiver.recv(), Some(1));
//!
//! sender.send(2);
//! sender.send(3);
//!
//! assert_eq!(receiver.recv(), Some(2));
//!
//! // clone the receiver
//! let mut receiver2 = receiver.clone();
//! assert_eq!(receiver.recv(), Some(3));
//! assert_eq!(receiver2.recv(), Some(3));
//! assert_eq!(receiver.recv(), None);
//! assert_eq!(receiver2.recv(), None);
//! # }
//! ```

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use crate::{core_impl, unpack_data_index, AtomicUsize, Node, Ordering, ReaderData, MAX_LEN};

#[cfg(not(loom))]
use alloc::sync::Arc;
#[cfg(loom)]
use loom::sync::Arc;

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
        core_impl::push(&self.buffer, &self.head, value);
    }

    fn pop(&self, reader: &mut ReaderData) -> Option<T> {
        core_impl::pop(&self.buffer, &self.head, reader)
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
/// # #[cfg(not(loom))]
/// # {
/// use blinkcast::alloc::channel;
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
/// # }
/// ```
/// Or using the [`new`](Sender::new) method:
/// ```
/// # #[cfg(not(loom))]
/// # {
/// use blinkcast::alloc::Sender;
///
/// let sender = Sender::<i32, 4>::new();
///
/// let mut receiver = sender.new_receiver();
///
/// sender.send(1);
/// sender.send(2);
/// assert_eq!(receiver.recv(), Some(1));
/// assert_eq!(receiver.recv(), Some(2));
/// assert_eq!(receiver.recv(), None);
/// # }
/// ```
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

    /// Creates a new channel with a buffer of size `N`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // TODO: use const_assert to check if N is a power of 2
        assert!(N <= MAX_LEN, "Exceeded the maximum length");

        Self {
            queue: Arc::new(InnerChannel::<T, N>::new()),
        }
    }

    /// Creates a new receiver that starts from the same point as the sender.
    ///
    /// # Examples
    /// ```
    /// # #[cfg(not(loom))]
    /// # {
    /// use blinkcast::alloc::Sender;
    ///
    /// let sender = Sender::<i32, 4>::new();
    ///
    /// sender.send(1);
    ///
    /// let mut receiver = sender.new_receiver();
    /// assert_eq!(receiver.recv(), None);
    ///
    /// sender.send(2);
    /// assert_eq!(receiver.recv(), Some(2));
    /// assert_eq!(receiver.recv(), None);
    /// # }
    /// ```
    pub fn new_receiver(&self) -> Receiver<T, N> {
        let head = self.queue.head.load(Ordering::Relaxed);
        let (lap, index) = unpack_data_index(head);

        Receiver {
            queue: self.queue.clone(),
            reader: ReaderData { index, lap },
        }
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
/// Can also be created with the [`new_receiver`](Sender::new_receiver) method of the [`Sender`].
///
/// This is a cloneable receiver, so you can have multiple receivers that start from the same
/// point.
///
/// Broadcast messages sent by the channel are received by the [`recv`](Receiver::recv) method.
///
/// # Examples
/// ```
/// # #[cfg(not(loom))]
/// # {
/// use blinkcast::alloc::channel;
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
/// # }
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
/// Another method to create a channel is using the [`Sender::new`] and [`Sender::new_receiver`] methods.
///
/// # Examples
/// ```
/// # #[cfg(not(loom))]
/// # {
/// use blinkcast::alloc::channel;
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
/// # }
/// ```
pub fn channel<T: Clone, const N: usize>() -> (Sender<T, N>, Receiver<T, N>) {
    let sender = Sender::<T, N>::new();
    let receiver = sender.new_receiver();
    (sender, receiver)
}
