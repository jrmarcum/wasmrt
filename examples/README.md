# examples/

Example guests + host-FFI demos for wasmrt — the Rust ports of wazmrt's `examples/`.

**Status: PREP placeholder.** No example files yet — the port gate is closed
(see `../cmem/roadmap.md`). This README lists what to port so coverage matches
the oracle. Most are compiled `wasm32-wasi` guests run *through* wasmrt (they are
compiler output, so they carry over unchanged); the FFI demo is re-pointed at
`wasmrt.h`.

## Guests to carry over (from wazmrt `examples/`)

| File | Proves |
|---|---|
| `hello_wasi.wat` | WASI p1 command: `fd_write` + `proc_exit` → "hello from wasi". |
| `hello_compiled.zig` | Real LLVM `wasm32-wasi` guest — `memory.copy` + saturating trunc + `fd_write`. |
| `c_hello.c` | C guest via `zig cc`/clang → printf→fd_write, sum 1..100. |
| `rust_hello.rs` | Rust guest via `rustc --target wasm32-wasip1` → sum of squares. |
| `wasi_files.zig` | Phase-3 filesystem: `path_open`/read/write/seek/readdir + 4 refused escapes + 1 allowed interior `..`. |
| `wasi_clock_stdin.zig` | `clock_res_get`, `poll_oneoff` sleep, stdin echo/EOF. |
| `wasi_leftovers.zig` | set-times, `fd_allocate`, `path_link`. |
| `wasi_symlink_traversal.zig` | The adversarial sandbox check — in-sandbox symlink followed, escapes refused, cycle→ELOOP. |

Most guests are toolchain output — they run through wasmrt unchanged and are used
by the `wasi-gate` conformance test (see `../tests/`).

## FFI demo (re-pointed at wasmrt)

- `deno_ffi.mjs` (port of wazmrt's) — `Deno.dlopen` the wasmrt **cdylib** and
  drive **`wasmrt.h`** (engine → module → linker/instantiate → call → read
  result) on `(func (export "answer") (result i32) (i32.const 42))` → `42`. This
  is the concrete "drop-in engine for the loaders" acceptance demo. Depends on the
  finalized `wasmrt.h` (held for review — see `../docs/port/wasmrt.h.draft`).

Reference originals: `../../wazmrt/examples/`.
