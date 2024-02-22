//! A channel implemented with static memory.
//!
//! This module implements a broadcast channel with a fixed-size buffer, without allocation.
//! The buffer (hosted by the [`Sender`]) is stored in static memory, it can be in the stack
//! or in global static variables, and this can be done because [`Sender::new`] is a `const fn`.
//!
//! When sending, we only need `&Sender`, so it can be done from multiple threads/cores as the same time.
//!
//! The channel overwrites the oldest message if the buffer is full, prioritizing the latest data.
//!
//! **Key Features:**
//!
//! * **Broadcast:** Multiple senders can send messages to multiple receivers simultaneously.
//! * **Fixed-size Buffer:** Provides bounded memory usage with predictable performance.
//! * **Overwriting Behavior:** Prioritizes the latest data in scenarios where the buffer becomes full.
//! * **Cloneable:** the `Receiver` are cloneable, enabling flexible message distribution patterns,
//!    the `Sender` is not cloneable, but if setup in a global static variable, it can be used from multiple locations.
//!
//! **Usage Considerations:**
//! * Well-suited for scenarios where multiple components need to broadcast messages and the latest data takes priority.
//! * Ideal when heap allocation is necessary or desirable.
//! * Receivers must be fast enough to keep up with the senders and avoid losing messages due to overwriting.

use core::mem::ManuallyDrop;

use crate::{
    core_impl, unpack_data_index, AtomicUsize, MaybeUninit, Node, Ordering, ReaderData, MAX_LEN,
};

struct InnerChannel<T, const N: usize> {
    buffer: [Node<T>; N],
    head: AtomicUsize,
}

impl<T: Clone + Sized, const N: usize> InnerChannel<T, N> {
    const fn new() -> Self {
        // Create an uninitialized array of `MaybeUninit`. The `assume_init` is
        // safe because the type we are claiming to have initialized here is a
        // bunch of `MaybeUninit`s, which do not require initialization
        let mut uninit_buffer: [MaybeUninit<Node<T>>; N] =
            unsafe { MaybeUninit::uninit().assume_init() };

        let mut i = 0;
        while i < N {
            uninit_buffer[i] = MaybeUninit::new(Node::<T>::empty());
            i += 1;
        }

        // Safety: we have initialized all the elements
        // This transmute_copy will copy again, not sure if it can be optimized by the compiler
        // but this is still an open issue (transmute doesn't work): https://github.com/rust-lang/rust/issues/61956
        // or use `MaybeUninit::array_assume_init` when it is stabilized
        #[repr(C)]
        union InitializedData<T, const N: usize> {
            uninit: ManuallyDrop<[MaybeUninit<Node<T>>; N]>,
            init: ManuallyDrop<[Node<T>; N]>,
        }
        let buffer = ManuallyDrop::into_inner(unsafe {
            InitializedData {
                uninit: ManuallyDrop::new(uninit_buffer),
            }
            .init
        });

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

/// The sender of the channel.
///
/// This is a the main channel component, as this is stored in static memory,
/// The `Sender` is the owner of the memory.
/// You can use it from multiple locations by storing it in a `static` variable.
///
/// `static_mem` doesn't have something like [`channel`](crate::alloc::channel) function,
/// Because, we don't have heap to store the Sender and give you an [`Arc`](https://doc.rust-lang.org/std/sync/struct.Arc.html)
/// to clone it. So, the user has to create the `receiver` from the `sender` manually.
///
/// Use [`new_receiver`](Sender::new_receiver) to create a receiver.
/// It will start from the same point as the sender.
///
/// Broadcast messages sent by using the [`send`](Sender::send) method.
///
/// # Examples
/// ```
/// # #[cfg(not(loom))]
/// # {
/// use blinkcast::static_mem::Sender;
///
/// let sender = Sender::<i32, 4>::new();
/// let mut receiver = sender.new_receiver();
///
/// sender.send(1);
/// sender.send(2);
///
/// assert_eq!(receiver.recv(), Some(1));
/// assert_eq!(receiver.recv(), Some(2));
/// assert_eq!(receiver.recv(), None);
/// # }
/// ```
pub struct Sender<T, const N: usize> {
    queue: InnerChannel<T, N>,
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
    pub const fn new() -> Self {
        // TODO: use const_assert to check if N is a power of 2
        assert!(N <= MAX_LEN, "Exceeded the maximum length");

        Self {
            queue: InnerChannel::<T, N>::new(),
        }
    }

    /// Creates a new receiver that starts from the same point as the sender.
    ///
    /// # Examples
    /// ```
    /// # #[cfg(not(loom))]
    /// # {
    /// use blinkcast::static_mem::Sender;
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
    pub fn new_receiver(&self) -> Receiver<'_, T, N> {
        let head = self.queue.head.load(Ordering::Relaxed);
        let (lap, index) = unpack_data_index(head);

        Receiver {
            queue: &self.queue,
            reader: ReaderData { index, lap },
        }
    }
}

impl<T: Clone, const N: usize> Default for Sender<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The receiver of the channel.
///
/// Can be created with the [`new_receiver`](Sender::new_receiver) method of the [`Sender`].
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
/// use blinkcast::static_mem::Sender;
///
/// let sender = Sender::<i32, 4>::new();
/// let mut receiver = sender.new_receiver();
///
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
pub struct Receiver<'a, T, const N: usize> {
    queue: &'a InnerChannel<T, N>,
    reader: ReaderData,
}

unsafe impl<T: Clone + Send, const N: usize> Send for Receiver<'_, T, N> {}
unsafe impl<T: Clone + Send, const N: usize> Sync for Receiver<'_, T, N> {}

impl<T: Clone, const N: usize> Receiver<'_, T, N> {
    /// Receives a message from the channel.
    ///
    /// If there is no message available, this method will return `None`.
    pub fn recv(&mut self) -> Option<T> {
        self.queue.pop(&mut self.reader)
    }
}

impl<T: Clone, const N: usize> Clone for Receiver<'_, T, N> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue,
            reader: self.reader.clone(),
        }
    }
}
