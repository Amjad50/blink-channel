# blinkcast

[![check](https://github.com/Amjad50/blinkcast/actions/workflows/check.yml/badge.svg)](https://github.com/Amjad50/blinkcast/actions/workflows/check.yml) 
[![codecov](https://codecov.io/gh/Amjad50/blinkcast/graph/badge.svg?token=I4ORM3HHCK)](https://codecov.io/gh/Amjad50/blinkcast)
[![Crates.io blinkcast](https://img.shields.io/crates/v/blinkcast)](https://crates.io/crates/blinkcast)
[![docs.rs blinkcast](https://docs.rs/blinkcast/badge.svg)](https://docs.rs/blinkcast)

Fast, Bounded, Lossy Rust broadcast channel with support for `no_std` targets.

> Sometimes it may spin the CPU for a bit if there is a contention on a single element in the buffer for write and read operations. Could happen more often for small buffers.

This is implemented with ring buffer and atomic operations, it may be considered lock-free, as we
don't use `Lock` premitive, but the implementation may spin waiting for a contending
writer/reader to finish accessing a specific node. Its very rare, but
maybe I won't call it `lock-free` in the strict sense.

The API of the `blinkcast` is similar to that of the `std::sync::mpsc` channels.
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

See [the documentation](https://docs.rs/blinkcast) for examples.

# Minimum Supported Rust Version (MSRV)
The minimum supported Rust version for this crate is `1.61.0`

# License
Licensed under `MIT` ([LICENSE](./LICENSE) or http://opensource.org/licenses/MIT)
