//! A channel implemented with heap allocated buffers (require `alloc` feature).

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use crate::{core_impl, AtomicUsize, Node, ReaderData, MAX_LEN};

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
    // TODO: replace with compile time assert
    assert!(
        N <= MAX_LEN,
        "The buffer size must be less than {}",
        MAX_LEN
    );

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
