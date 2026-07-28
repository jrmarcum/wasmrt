# Changelog

All notable changes to wasmrt are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and wasmrt uses a **port-progress**
versioning scheme: `0.x` releases climb toward **1.0.0 = full parity** with the
[`wazmrt`](https://github.com/jrmarcum/wazmrt) reference oracle. See [ROADMAP.md](ROADMAP.md) for the
stage ladder and the live use-case matrix.

The three crates share one version and are released together: `wasmrt` (CLI), `wasmrt-core` (library),
`wasmrt-capi` (C ABI).

## [Unreleased]

_Next: 0.3.0 — the shared opcode IR table (T2)._

## [0.2.0] — Types + reader (stage T1)

The decode primitives, ported from the wazmrt oracle and verified 1:1 against its test vectors.

### Added
- `wasmrt-core::types` — WebAssembly value types as a `ValType` `u32` newtype with concrete typed
  references bit-packed in the high bits (bit 31 concrete, bit 30 nullable, bits 28–29 family, bits 0–27
  index); `RefHeap` with the WasmGC subtyping relation; `SectionId`, `ExternKind`, and the `DecodeError`
  set.
- `wasmrt-core::reader` — a zero-copy `Reader` cursor: bounds-checked reads and spec-correct LEB128
  (`u32`/`u64`/`i32`/`i64`/`s33`) that reject over-long encodings and out-of-range values (§5.2.2), plus
  fixed little-endian and float-bit reads.
- Test vectors ported from wazmrt (LEB accept/reject edges) plus ValType bit-packing and subtyping tests.
  All modules stay `no_std` and compile libc-free to `wasm32`.

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

[Unreleased]: https://github.com/jrmarcum/wasmrt/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jrmarcum/wasmrt/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jrmarcum/wasmrt/releases/tag/v0.1.0
