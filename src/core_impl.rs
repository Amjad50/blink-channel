use core::cmp;

use crate::{
    is_readable, is_reading, pack_data_index, unpack_data_index, AtomicUsize, MaybeUninit, Node,
    Ordering, ReaderData, STATE_AVAILABLE, STATE_EMPTY, STATE_READING, STATE_WRITING,
};

pub(crate) fn push<T>(buffer: &[Node<T>], head: &AtomicUsize, value: T) {
    let mut node;
    let mut should_drop;

    let mut current_head = head.load(Ordering::Acquire);

    loop {
        let (mut producer_lap, mut producer_index) = unpack_data_index(current_head);

        node = &buffer[producer_index % buffer.len()];

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
                STATE_WRITING => {
                    // move to next position
                    producer_index += 1;
                    if producer_index == 0 {
                        producer_lap += 1;
                    }
                    current_head = pack_data_index(producer_lap, producer_index);
                    node = &buffer[producer_index % buffer.len()];
                }
                s => unreachable!("Invalid state: {}", s),
            }
        }

        let next_index = (producer_index + 1) % buffer.len();
        let next_lap = if next_index == 0 {
            producer_lap + 1
        } else {
            producer_lap
        };
        let next_head = pack_data_index(next_lap, next_index);

        match head.compare_exchange(current_head, next_head, Ordering::AcqRel, Ordering::Relaxed) {
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

pub(crate) fn pop<T: Clone>(
    buffer: &[Node<T>],
    head: &AtomicUsize,
    reader: &mut ReaderData,
) -> Option<T> {
    let (producer_lap, producer_index) = unpack_data_index(head.load(Ordering::Acquire));
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
                let new_index = producer_index % buffer.len();
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
    let mut node = &buffer[reader_index % buffer.len()];

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
                node = &buffer[reader_index % buffer.len()];
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
        return pop(buffer, head, reader);
    }

    let data = unsafe { node.data.get().read().assume_init_ref().clone() };

    reader.index = (reader_index + 1) % buffer.len();
    if reader.index == 0 {
        reader.lap += 1;
    }

    node.state.fetch_sub(STATE_READING, Ordering::Release);

    Some(data)
}
