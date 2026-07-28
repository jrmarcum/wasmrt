# Changelog

All notable changes to wasmrt are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and wasmrt uses a **port-progress**
versioning scheme: `0.x` releases climb toward **1.0.0 = full parity** with the
[`wazmrt`](https://github.com/jrmarcum/wazmrt) reference oracle. See [ROADMAP.md](ROADMAP.md) for the
stage ladder and the live use-case matrix.

The three crates share one version and are released together: `wasmrt` (CLI), `wasmrt-core` (library),
`wasmrt-capi` (C ABI).

## [Unreleased]

_Next: 0.2.0 — types + reader (value/reference types, spec-correct LEB128), parity-gated against wazmrt._

## [0.1.0] — Scaffold (stage T0)

First release: the crate exists and the build is real on every target surface. No runtime yet.

### Added
- Three-crate Cargo workspace: `wasmrt-core` (no_std-friendly runtime library), `wasmrt-capi`
  (`staticlib` + `cdylib` C ABI, ships `wasmrt.h`), and `wasmrt` (the CLI binary).
- `wasmrt-core` module tree stubbed to mirror the wazmrt source layout, populated bottom-up in later
  releases.
- C ABI version handshake (`wasmrt_abi_version`) proving the `extern "C"` boundary links as both a
  static and a shared library.
- Size-first release profile; builds verified on native (CLI + static lib + cdylib) and freestanding
  `wasm32-unknown-unknown` (no_std, libc-free).

[Unreleased]: https://github.com/jrmarcum/wasmrt/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jrmarcum/wasmrt/releases/tag/v0.1.0
