# examples/

Example guests + host-FFI demos for wasmrt — the Rust ports of wazmrt's `examples/`.

**Status (2026-08-06): still no example files — and every blocker is gone.** The
port gate opened 2026-07-27 and T0–T8 are done: wasmrt runs WASI preview 1 with a
sandboxed filesystem (v0.8.0) and ships a finalized C ABI (v0.9.0), so **every
item below is now buildable**. This is a real gap, not a placeholder.

Note the coverage that already exists elsewhere, so effort is not duplicated: the
**wasmtk WASI corpus** (376 modules, run end-to-end against the oracle) and the
**vendored spec testsuite** cover the guest side, and `../tests/c_smoke.c` already
drives the C ABI from C. What is genuinely missing here is the **embedder-facing
demo** — the "drop-in engine for the loaders" acceptance case.

Most guests below are toolchain output, so they carry over unchanged.

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
  is the concrete "drop-in engine for the loaders" acceptance demo.

  **✅ UNBLOCKED — the header was finalized and shipped at T8 / v0.9.0**:
  [`../crates/wasmrt-capi/include/wasmrt.h`](../crates/wasmrt-capi/include/wasmrt.h).
  *(`../docs/port/wasmrt.h.draft` is marked HISTORICAL — do not write against it;
  four things in it never matched the shipped surface.)* Two contract points a
  `dlopen`-style consumer must respect: check `wasmrt_abi_version()` against the
  header's `WASMRT_ABI_VERSION` at load time, and treat a **guest trap** (the
  `trap_out` parameter) as distinct from a **host-side error** (the returned
  `wasmrt_error_t *`) — a call can return NULL and still have trapped.

Reference originals: `../../wazmrt/examples/`.
