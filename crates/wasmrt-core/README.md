# wasmrt-core

The runtime core of [wasmrt](https://github.com/jrmarcum/wasmrt): decode, validate and interpret
WebAssembly, plus the text toolchain (`.wat` assembler, `.wast` script runner) and WASI preview 1.

- **`#![forbid(unsafe_code)]`** and **zero third-party dependencies**.
- `no_std`-friendly: build with `--no-default-features` for a freestanding `wasm32` target.
- Covers integer/float compute, linear memory (incl. multi-memory and memory64), tables and
  reference types, WasmGC, the full `v128` SIMD set, atomics, and exception handling.

Embedding from C? Use [`wasmrt-capi`](https://crates.io/crates/wasmrt-capi). Want the command-line
tool? Use [`wasmrt`](https://crates.io/crates/wasmrt).

`SPDX-License-Identifier: MIT OR Apache-2.0`
