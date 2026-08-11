# wasmrt

A fast, small, idiomatic-**Rust** WebAssembly runtime.

wasmrt is a from-scratch Rust port of the Zig runtime [`wazmrt`](https://github.com/jrmarcum/wazmrt) —
which already runs the full WebAssembly spec testsuite and a large WASI corpus. wazmrt is the
**reference oracle**; wasmrt is finished when it reproduces the oracle feature-for-feature, verified by
parity testing at every step.

> **Status: early — `0.9.0`.** wasmrt can **assemble**, **decode**, **type-check**, and **run**
> WebAssembly — `wasmrt run fac.wasm fac 10` → `3628800`, `wasmrt wat mod.wat -o mod.wasm`. Execution
> covers integer + floating-point compute, linear memory (incl. **multi-memory** and **memory64**),
> tables / `call_indirect` / reference types, **WasmGC**, **SIMD** (the full `v128` set, incl. relaxed),
> **atomics**, and **exception handling** — and the type-checker covers all of it, so
> `wasmrt <file>` gives a real verdict on any module wasmrt can run. Every command that takes a
> module accepts either form — `wasmrt run fac.wat fac 10` assembles the text first, then runs it.
>
> It reads and writes the **text format** and runs the **official spec testsuite**: `wasmrt wast <dir>`
> scores **99.4% (62,113 assertions passing)**, with every one of the 284 files parsing.
>
> It is **embeddable from C** via [`wasmrt.h`](crates/wasmrt-capi/include/wasmrt.h) — compile,
> link host functions, instantiate, call exports, and read or write guest memory. You can restrict
> which WebAssembly proposals a guest may use and cap the memory, call depth and GC objects it may
> consume. Handles are checked, so one from the wrong store is refused rather than followed.
>
> It also runs **real WASI programs** — `wasmrt wasi prog.wasm` — with stdio, args, environ, clocks,
> `random_get` and a **sandboxed filesystem**. A guest reaches only what you preopen:
> `--dir <host>[::<guest>]`, or `--ro-dir` for read-only (which propagates to the whole subtree). With
> no `--dir`, every path call returns `BADF` — there is no implicit working directory.
>
> The engine is **`#![forbid(unsafe_code)]`**: `wasmrt-core` and the CLI contain no `unsafe` at all, and
> the compiler enforces it. The C ABI cannot be — a foreign boundary is unsafe by definition — so every
> raw pointer it touches goes through one small audited module, and the whole surface runs under
> **Miri**. See **[ROADMAP.md](ROADMAP.md)** for the live use-case matrix (what actually works today)
> and **[CHANGELOG.md](CHANGELOG.md)** for release notes.

## Goals

- **Canonical** — run the same WebAssembly `wasmtime` can (full browser-standard feature set + memory64;
  WASI preview 1). **Tail calls are the one scope item not yet implemented.**
- **Fast** — win cold-start and native-FFI workloads (an interpreter over a pre-decoded IR, not a JIT).
- **Small** — minimize every artifact; the runtime even compiles to `wasm32` to embed inside another
  wasm host.

**Measured, not claimed** (`cargo run --release -p wasmrt-core --example bench`; x86_64, release):

| | |
| --- | --- |
| Cold start (decode + validate + instantiate + call, 48 KB module) | **4.5 ms** |
| Steady-state dispatch (tight `loop`/`br_if`) | **~237 Mops/s** |
| CLI binary · C-ABI `cdylib` | **621 KiB** · **494 KiB** |
| Runtime compiled to `wasm32`, engine only | **158 KiB** (**138 KiB** after `wasm-opt -Oz`) |

Sustained hot loops go to a JIT — that trade is deliberate. A comparison against other small runtimes
(wasm3, WAMR) has **not** been run yet, so no claim is made about it.

## Crates

| Crate | What it is | Install |
| --- | --- | --- |
| `wasmrt` | the command-line runtime | `cargo install wasmrt` |
| `wasmrt-core` | the runtime as a Rust library (`no_std`-friendly) | `wasmrt-core = "0.9"` |
| `wasmrt-capi` | the C ABI ([`wasmrt.h`](crates/wasmrt-capi/include/wasmrt.h)) as a `cdylib` / `staticlib` | build from source |

## Embedding from C

`cargo build -p wasmrt-capi --release` produces a `staticlib` and a `cdylib`; the header is
[`crates/wasmrt-capi/include/wasmrt.h`](crates/wasmrt-capi/include/wasmrt.h).

```c
wasmrt_engine_t *engine = wasmrt_engine_new();
wasmrt_store_t  *store  = wasmrt_store_new(engine);
wasmrt_linker_t *linker = wasmrt_linker_new(engine);

wasmrt_module_t *module = NULL;
wasmrt_error_t  *err = wasmrt_module_new(engine, bytes, len, &module);   /* NULL == success */

wasmrt_instance_t inst;  wasmrt_trap_t *trap = NULL;
wasmrt_linker_instantiate(linker, store, module, &inst, &trap);

wasmrt_func_t add;
wasmrt_instance_get_func(store, inst, "add", &add);
wasmrt_val_t args[2] = {{WASMRT_I32, {.i32 = 40}}, {WASMRT_I32, {.i32 = 2}}};
wasmrt_val_t result;
wasmrt_func_call(store, add, args, 2, &result, 1, &trap);                /* result.of.i32 == 42 */
```

Three things worth knowing before you write against it:

- **A trap is not an error.** A host-side error (bad handle, wrong arity) comes back as a
  `wasmrt_error_t *`; a *guest* trap arrives through the `trap_out` parameter. A call can return `NULL`
  and still have trapped — check both.
- **Handles are checked, not trusted.** `wasmrt_func_t` and friends are small copyable values you never
  free. Each knows which store issued it, so one used with the wrong store is refused rather than
  quietly naming a different function.
- **`wasmrt_memory_data()` is invalidated** by anything that can grow memory or re-enter the guest.
  Re-fetch it after each call, or use the bounds-checked `wasmrt_memory_read`/`_write` instead.

## Building from source

```sh
cargo build --workspace          # native: CLI + static lib + cdylib
cargo test  --workspace
cargo build -p wasmrt-core --no-default-features --target wasm32-unknown-unknown  # freestanding

scripts/c-gate.sh                # compile + run the C-ABI gates against the shipped header
scripts/miri-gate.sh             # the C ABI under Miri (needs: rustup component add miri)
```

On Windows, use the `x86_64-pc-windows-gnullvm` toolchain host (LLVM-MinGW + UCRT — libc-free, no MSVC).

## Versioning

wasmrt uses a **port-progress** scheme: `0.x` releases each land a parity-gated stage, and **`1.0.0`
means full parity** with the frozen wazmrt oracle. The number reflects what genuinely runs today.

## License

`SPDX-License-Identifier: MIT OR Apache-2.0`

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
wasmrt has **no third-party dependencies** and incorporates no third-party code — see
[`NOTICE`](NOTICE) and [`third_party/LICENSES.md`](third_party/LICENSES.md), whose Component Ledger is
empty.
