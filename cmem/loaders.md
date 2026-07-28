# Loaders — the consumers that drive `wasmrt.h`

wasmrt's reason to exist: be the engine the **`universalWasmLoader-*`** projects run on, replacing
wasmtime. Full survey: `docs/port/07-loaders-api.md` + `docs/port/08-loader-survey-consolidated.md`.
Loaders live at `../../GithubProjects/universalWasmLoader` (10 language loaders).

## Architecture (reconciles the "canonical ABI" language)

Each loader is a HIGH-LEVEL library that loads a **core `.wasm` + a `.wit` sidecar** produced by
`wasmtk` (TS→wasm) and applies the **Component-Model Canonical ABI ITSELF** over that core module —
writing string args into guest memory via the module's exported **`cabi_realloc`**, reading
callee-allocated `[ptr,len]` returns, calling **`cabi_post_<name>`** (their "SPEC v3.0.0"). No loader
uses any component-model API. **That marshalling is the LOADER's job — NOT the runtime's.** So
`wasmrt.h` provides only the **engine primitives** the loaders call; the Canonical ABI stays in the
loaders. (This is what the original kickoff prompt's `cabi_realloc`/`cabi_post_*`/WIT language was
about — it belongs here, not in the runtime.)

## `wasmrt.h` = the engine surface the loaders call (~38 fns)

The C loader (`universalWasmLoader-c/universal_wasm_loader.h`, `uwl_*` public API) uses wasmtime's
**`wasmtime_*` store/context/linker/typed-val model** (not the standard wasm-c-api instance/func
model). The union wasmrt.h must expose:

- Engine + compile module (from bytes); store/context with per-instance memory.
- **Linker:** host-func define, `define_wasi`, unknown-imports-as-traps, instantiate.
- WASI **preview 1** subset actually imported: `fd_write` (iovec→stdio), `proc_exit`, `random_get`,
  `clock_time_get`, `environ/args` sizes+get, `fd_close`, `fd_fdstat_get`, `fd_seek`. Config = inherit
  stdout/stderr (preopens/env/args optional adds).
- Reactor `_initialize` once post-instantiate.
- Export invoke by name: typed args, **0 or 1 result** (typed fast path for `cabi_realloc`/`cabi_post`/
  `__malloc`).
- **Raw linear-memory `data` + `data_size`** — the loaders marshal by hand, including **from inside a
  host callback via a Caller/`caller_export_get("memory")` handle.**
- Exported global read; traps/errors with messages.

**NOT needed by any loader** (omit from wasmrt.h, still full parity): tables/indirect, **multi-value
returns**, `memory.grow`, threads, SIMD host-calls, the whole component API. *(The engine still
EXECUTES full-parity modules — a module may use SIMD internally; the host API just never exposes it.)*

## The load-bearing gap over wazmrt's shape

wazmrt's C ABI is the standard wasm-c-api, whose host-func callback gets **no handle to the caller's
memory**. But essentially every loader's host imports need exactly that — read guest memory in a
callback and return a value. So **`wasmrt.h` adopts wasmtime's caller-based callback model**
(`wasmrt_caller_get_memory`). The interp layer already supports it (`HostFunc.native_env` ctx); the
C-ABI surface must expose it.

## ABI strategy + targets (decided 2026-07-17)

- **Strategy:** clean lean **`wasmrt_*` C ABI (wasmtime-*shaped*, our names) + a native `wasmrt` Rust
  crate.** No exact-wasmtime-symbol compat shim. Owner owns the loaders and updates them.
- **Draft:** `docs/port/wasmrt.h.draft` (v0, ~40 fns). The "held until wazmrt finalizes" review-block is
  **lifted** (oracle frozen 2026-07-27) → finalize the header with the owner at the **C-ABI task (T8** in
  `roadmap.md`) before writing `wasmrt-capi`.

Three substrates today; **all 10 loaders are eventual targets, phased by effort:**

| Phase | Loaders | Mechanism |
|---|---|---|
| 1 | **c, v, zig** + **rs** | `wasmrt.h` C ABI (v/zig via the C loader) + a native `wasmrt` Rust crate for rs (free — wasmrt is Rust). |
| 2 | **dotnet, py** | thin bindings over `wasmrt.h` (replace the Wasmtime-NuGet / wasmtime-py packages). |
| 3 | **go** (was wazero), **jvm** (was Chicory) | FFI `wasmrt.h` via cgo / JNI-or-Panama. |
| 4 | **js, dart** | run guests through **wasmrt compiled to wasm32** (wasm-in-wasm) — the embed-inside-a-wasm-host vision; heaviest, last. |
