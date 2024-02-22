# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2024-02-22

### Added
- feature `alloc` that contains the old implementation that relies on allocation.
- module `static_mem` that implements the channel without allocation, but known compile time size.
- `Sender::new` to create sender, and `Sender::new_receiver` to a receiver from the sender that will
  start receiving from the next message of the sender/s.

### Changed
- The `alloc` (previous) implementation doesn't need to specify size at compile time.
- Needed to bump MSRV to `1.61.0` to support some `const fn` features.

## [0.1.1] - 2024-02-22

### Fixed
- Race conditions on send (write) and receive (read) operations.

## [0.1.0] - 2024-02-21

### Added

- Initial implementation of the library.
- Tests and benches.
- Documentation.

[unreleased]: https://github.com/Amjad50/blinkcast/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Amjad50/blinkcast/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Amjad50/blinkcast/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Amjad50/blinkcast/compare/a9761ca3c16404ffd8c00efe0ed26fa377bb444d...v0.1.0
