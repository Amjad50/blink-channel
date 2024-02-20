#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, feature(test))]

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use core::{cell::UnsafeCell, cmp, fmt, mem::MaybeUninit};

#[cfg(loom)]
use loom::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[cfg(not(loom))]
use alloc::sync::Arc;
#[cfg(not(loom))]
use core::sync::atomic::{AtomicUsize, Ordering};

const DATA_EMPTY: usize = 0;
const DATA_AVAILABLE: usize = 1;
const DATA_WRITING: usize = 2;
// reading can be done by multiple readers
// so we use value that is power of 2
// we add this value to the state, which means there is a reader
const DATA_READING: usize = 4;
const READING_MASK: usize = usize::MAX & !(DATA_READING - 1);

fn is_read_state(state: usize) -> bool {
    state & READING_MASK != 0
}

struct Node<T> {
    data: UnsafeCell<MaybeUninit<T>>,
    state: AtomicUsize,
}

impl<T> fmt::Debug for Node<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Ordering::Relaxed);
        write!(f, "Node {{ state: {} }}", state)
    }
}

impl<T> Default for Node<T> {
    fn default() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(DATA_EMPTY),
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

            while is_read_state(state) {
                core::hint::spin_loop();
                // wait until the reader is done
                state = node.state.load(Ordering::Acquire);
            }

            match state {
                DATA_EMPTY | DATA_AVAILABLE => {
                    if node
                        .state
                        .compare_exchange_weak(
                            state,
                            DATA_WRITING,
                            Ordering::Release,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        should_drop = state == DATA_AVAILABLE;
                        break;
                    }
                }
                DATA_WRITING => unreachable!("There should be no writer writing"),
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
        node.state.store(DATA_AVAILABLE, Ordering::Release);
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

            if is_read_state(state) {
                let old = node.state.fetch_add(DATA_READING, Ordering::AcqRel);

                if old == state {
                    break;
                } else {
                    match old {
                        DATA_AVAILABLE => {
                            break;
                        }
                        _ => {
                            // something happened, rollback
                            // subtract one reader
                            node.state.fetch_sub(DATA_READING, Ordering::AcqRel);
                        }
                    }
                }
            }

            match state {
                DATA_AVAILABLE => {
                    let old = node.state.fetch_add(DATA_READING, Ordering::AcqRel);

                    if old == state || is_read_state(old) {
                        break;
                    } else {
                        // subtract one reader
                        node.state.fetch_sub(DATA_READING, Ordering::AcqRel);
                    }
                }
                DATA_WRITING => {
                    reader_index += 1;
                    node = &self.buffer[reader_index % self.buffer.len()];
                }
                DATA_EMPTY => unreachable!("There should be some data at least"),
                s => unreachable!("Invalid state: {}", s),
            }
        }

        let data = unsafe { node.data.get().read().assume_init_ref().clone() };

        reader.index = (reader_index + 1) % self.buffer.len();
        if reader.index == 0 {
            reader.lap += 1;
        }

        node.state.fetch_sub(DATA_READING, Ordering::Release);

        Some(data)
    }
}

pub struct Sender<T, const N: usize> {
    queue: Arc<InnerChannel<T, N>>,
}

impl<T: Clone, const N: usize> Sender<T, N> {
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

pub struct Receiver<T, const N: usize> {
    queue: Arc<InnerChannel<T, N>>,
    reader: ReaderData,
}

impl<T: Clone, const N: usize> Receiver<T, N> {
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

    #[cfg(loom)]
    use loom::thread;
    #[cfg(not(loom))]
    use std::thread;

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
    fn test_clone() {
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
            receivers.push(thread::spawn(move || {
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

#[cfg(all(test, not(loom)))]
mod bench {
    use super::*;

    use std::thread;

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

            let sender_thread = thread::spawn(move || {
                sender.send(1);
                sender.send(2);
                sender.send(3);
                sender.send(4);
            });

            let recv_thread = thread::spawn(move || {
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
