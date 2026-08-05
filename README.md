# wasmrt

A fast, small, idiomatic-**Rust** WebAssembly runtime.

wasmrt is a from-scratch Rust port of the Zig runtime [`wazmrt`](https://github.com/jrmarcum/wazmrt) —
which already runs the full WebAssembly spec testsuite and a large WASI corpus. wazmrt is the
**reference oracle**; wasmrt is finished when it reproduces the oracle feature-for-feature, verified by
parity testing at every step.

> **Status: early — `0.8.0`.** wasmrt can **assemble**, **decode**, **type-check**, and **run**
> WebAssembly — `wasmrt run fac.wasm fac 10` → `3628800`, `wasmrt wat mod.wat -o mod.wasm`. Execution
> covers integer + floating-point compute, linear memory (incl. **multi-memory** and **memory64**),
> tables / `call_indirect` / reference types, **WasmGC**, **SIMD** (the full `v128` set, incl. relaxed),
> **atomics**, and **exception handling** — and the type-checker covers all of it, so
> `wasmrt <file.wasm>` gives a real verdict on any module wasmrt can run.
>
> It reads and writes the **text format** and runs the **official spec testsuite**: `wasmrt wast <dir>`
> scores **98.8% (61,013 assertions passing)**, with every one of the 284 files parsing.
>
> It also runs **real WASI programs** — `wasmrt wasi prog.wasm` — with stdio, args, environ, clocks,
> `random_get` and a **sandboxed filesystem**. A guest reaches only what you preopen:
> `--dir <host>[::<guest>]`, or `--ro-dir` for read-only (which propagates to the whole subtree). With
> no `--dir`, every path call returns `BADF` — there is no implicit working directory.
>
> The engine is **`#![forbid(unsafe_code)]`**: `wasmrt-core` and the CLI contain no `unsafe` at all, and
> the compiler enforces it. See **[ROADMAP.md](ROADMAP.md)** for the live use-case matrix (what actually
> works today) and **[CHANGELOG.md](CHANGELOG.md)** for release notes.

## Goals

- **Canonical** — run the same WebAssembly `wasmtime` can (full browser-standard feature set + memory64;
  WASI preview 1).
- **Fast** — win cold-start and native-FFI workloads (an interpreter over a pre-decoded IR, not a JIT).
- **Small** — minimize every artifact; the runtime even compiles to `wasm32` to embed inside another
  wasm host.

## Crates

| Crate | What it is | Install |
| --- | --- | --- |
| `wasmrt` | the command-line runtime | `cargo install wasmrt` _(from 0.1.0)_ |
| `wasmrt-core` | the runtime as a Rust library (`no_std`-friendly) | `wasmrt-core = "0.1"` |
| `wasmrt-capi` | the C ABI (`wasmrt.h`) as a `cdylib` / `staticlib` | build from source |

## Building from source

```sh
cargo build --workspace          # native: CLI + static lib + cdylib
cargo test  --workspace
cargo build -p wasmrt-core --no-default-features --target wasm32-unknown-unknown  # freestanding
```

On Windows, use the `x86_64-pc-windows-gnullvm` toolchain host (LLVM-MinGW + UCRT — libc-free, no MSVC).

## Versioning

wasmrt uses a **port-progress** scheme: `0.x` releases each land a parity-gated stage, and **`1.0.0`
means full parity** with the frozen wazmrt oracle. The number reflects what genuinely runs today.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
