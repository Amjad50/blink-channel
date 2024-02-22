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
#[should_panic]
fn test_channel_too_large() {
    loom!({
        let _ = channel::<i32, { MAX_LEN + 1 }>();
    });
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
// FIXME: spin panic on loom
#[cfg(not(loom))]
fn test_always_overflow() {
    let (sender, mut receiver) = channel::<i32, 4>();

    for i in 0..100 {
        sender.send(i);
    }

    for i in 100 - 4..100 {
        assert_eq!(receiver.recv(), Some(i));
    }
    assert_eq!(receiver.recv(), None);
}

#[test]
// FIXME: spin panic on loom
#[cfg(not(loom))]
fn test_sender_receiver_conflict() {
    let (sender, receiver) = channel::<i32, 4>();

    let barrier = Arc::new(std::sync::Barrier::new(2));

    for _ in 0..10 {
        // setup
        // fill the channel
        for i in 0..4 {
            sender.send(i);
        }

        // send and receive exactly at the same time
        let s_handle;
        let r_handle;
        {
            let barrier = barrier.clone();
            let mut receiver = receiver.clone();
            r_handle = std::thread::spawn(move || {
                barrier.wait();
                let v = receiver.recv();
                assert!(v == Some(0) || v == Some(1), "v = {v:?}");
            });
        }
        {
            let barrier = barrier.clone();
            let sender = sender.clone();
            s_handle = std::thread::spawn(move || {
                barrier.wait();
                sender.send(5);
            });
        }

        // wait for the threads to finish
        s_handle.join().unwrap();
        r_handle.join().unwrap();
    }
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
