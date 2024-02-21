# Blink-channel

[![check](https://github.com/Amjad50/blink-channel/actions/workflows/check.yml/badge.svg)](https://github.com/Amjad50/blink-channel/actions/workflows/check.yml) [![codecov](https://codecov.io/gh/Amjad50/blink-channel/graph/badge.svg?token=I4ORM3HHCK)](https://codecov.io/gh/Amjad50/blink-channel)

Fast, Lock-free, Bounded, Lossy Rust broadcast channel.

> Sometimes it may spin the CPU for a bit if there is a contention on a single element in the buffer for write and read operations. Could happen more often for small buffers.

This is implemented with ring buffer and atomic operations, which provide us with lock-free behavior with
no extra dependencies.

The API of the `blink-channel` is similar to that of the `std::sync::mpsc` channels.
However, there are some differences:

- It allows for multiple consumers (receivers) and multiple prodocuers (senders).
- The channel broadcasts every send to every consumer.
- Lossy, the sender will overwrite old data, so receivers must be quick or they will lose the old data (don'
t blink).
- Implemented for `no_std` environments.

Due to that nature, this is useful in applications where data comes in very quick and new data is always more important
than old data which can be discarded. 
This could be useful for example in implementing audio driver, where a small glitch but staying
up to date is better than delayed audio.

# Minimum Supported Rust Version (MSRV)
The minimum supported Rust version for this crate is `1.60.0`

# License
Licensed under `MIT` ([LICENSE](./LICENSE) or http://opensource.org/licenses/MIT)
