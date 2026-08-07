# wasmrt (CLI)

The command-line tool of [wasmrt](https://github.com/jrmarcum/wasmrt), a fast, small,
idiomatic-Rust WebAssembly runtime.

```sh
wasmrt module.wasm                 # summarize + type-check
wasmrt run fac.wasm fac 10         # call an export
wasmrt wasi prog.wasm --dir .::/   # run a WASI preview-1 program in a sandbox
wasmrt wat mod.wat -o mod.wasm     # assemble the text format
wasmrt wast tests/                 # run official spec-testsuite scripts
```

With no `--dir`, every WASI path call returns `BADF` — there is no implicit working directory.

The library is [`wasmrt-core`](https://crates.io/crates/wasmrt-core); the C ABI is
[`wasmrt-capi`](https://crates.io/crates/wasmrt-capi).

`SPDX-License-Identifier: MIT OR Apache-2.0`
