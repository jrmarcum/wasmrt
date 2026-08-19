# Consumers — who links wasmrt, and through which surface

wasmrt's reason to exist: be the engine the **`universalWasmLoader-*`** projects run on, replacing
wasmtime. Full survey: `docs/port/07-loaders-api.md` + `docs/port/08-loader-survey-consolidated.md`.
Loaders live at `../../GithubProjects/universalWasmLoader` (10 language loaders).

## 🔒 TWO consumer surfaces, not one (owner, 2026-08-11)

Until now this file assumed every consumer arrives through `wasmrt.h`. That is no longer true, and the
distinction is **load-bearing for the size and speed work** — the two surfaces have different levers, so
a win in one can be invisible in the other (`vision.md`).

| consumer | links | surface | notes |
| --- | --- | --- | --- |
| **`rsxtk`** | `wasmrt-core` **rlib** | **native Rust API** — no C ABI, no cdylib, no FFI | 🆕 **Takes wasmrt BY DEFAULT** (owner). Rust-to-Rust, so calls are direct and the C ABI is not in the path at all. |
| **`wasmtk`**, **`universalWasmLoader-*`** | `wasmrt_capi` cdylib / staticlib | **`wasmrt.h`** (~38 fns, below) | wasmrt is an **entrant competing for inclusion** here, judged on smallest + fastest binary — not a foregone conclusion. |

⚠️ **What this changes for optimization work:** for rsxtk the number that matters is *what LTO leaves in
rsxtk's final binary* — cross-crate dead-code elimination, generic bloat, and whether unused subsystems
(the WAT assembler, the `.wast` runner, WASI) actually drop out when unused. For the C consumers it is
the **shipped 493.5 KiB cdylib** plus per-call boundary overhead, and **nothing** is dead-code-eliminated
because every exported symbol is reachable by definition. ⚠️ **The rlib figure has never been measured**,
which means the *default* consumer's artifact is the one with no number on it — logged as T11 work.

## `rsxtk` — READ, not assumed (2026-08-11)

**`D:\Programs\_ProgramExamples\Example_Programs\Rust\rsxtk`** — v0.4.4, MIT/Apache, edition 2024,
`github.com/jrmarcum/rsxtk`. *"A high-performance Rust WASM Toolkit for managing and running WASI
scripts, WAT, and WASM modules."* **One source file, `src/main.rs`, 463 lines.** Release profile:
`lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`, `opt-level = 3`.

**It runs on `wasmtime` 40.0.1 today** (`default-features = false`, features `cranelift`, `runtime`,
`component-model`) plus `wasmtime-wasi` 40.0.1 (`p1`). **wasmrt's job is to replace those two.**

The engine surface it actually touches is **narrow** — this is the whole of it:

| rsxtk uses | wasmrt equivalent | status |
| --- | --- | --- |
| `Engine`, `Store`, `Module`, `Linker`, `Val`, `ValType` | `interp::{Instance, Store, Value}`, `module::Module`, `validate` | ✅ present |
| `Config`, `OptLevel`, `cranelift_opt_level` | — | n/a: **no JIT to configure** |
| `wasmtime_wasi::WasiCtxBuilder`, `p1::{self, WasiP1Ctx}` | `wasi::WasiCtx`, `wasi::link` | ✅ present |
| `preopened_dir(.., DirPerms::all(), FilePerms::all())` | `WasiCtx::preopen_dir(.., rights)` | ✅ present — and **stricter by default** (symlink *creation* denied unless opted in) |
| `wasmtime_wasi::I32Exit` downcast | `shared.borrow().exit_code()` | ✅ present |
| `wat::parse_bytes` (the `wat` crate) | `wasmrt_core::wat::assemble` | ✅ present — corpus figure UNVERIFIED, see `testing.md` (2026-08-19) |
| `wasmprinter::print_bytes` (wasm → wat) | — | ❌ **wasmrt has no printer** |
| `walrus::Module::from_buffer` (×2, module rewriting) | — | ❌ **wasmrt has no rewriter** |

⚠️ **The two ❌ rows are NOT blockers and must not be treated as scope.** `wasmprinter` and `walrus` are
*toolkit* functions — printing and rewriting modules for rsxtk's own subcommands — not engine functions.
They stay as rsxtk dependencies whichever runtime wins. Only `wasmtime` + `wasmtime-wasi` are in play.

🆕 **`component-model` is enabled in rsxtk's `Cargo.toml` and NOT used anywhere in `src/`** — zero
component APIs, verified by grep. It is a dead feature flag paying compile time and binary size for
nothing. **This matters because it looks like a blocker and is not:** wasmrt excludes the component model
by owner decision (`vision.md` — WASI stays preview 1), so an unread `Cargo.toml` would suggest rsxtk
needs something wasmrt will never have. It doesn't. *Worth telling the rsxtk owner regardless.*

### 🔒 `.cwasm` will NOT be the default — plain `.wasm` will, for cross-platform compatibility (owner, 2026-08-11)

rsxtk currently leans on `.cwasm` hard: `engine.precompile_module()` writes a `<stem>.cwasm` into a cache
keyed by mtime, and loads it back through **`unsafe { Module::deserialize_file(..) }`** (plus a `"cwasm"`
extension arm). The owner is demoting that path.

**This is a decision that favours wasmrt, and the reasoning generalizes:**

- A `.cwasm` is **target-specific machine code** — bound to the host ISA *and* to the exact wasmtime
  version that produced it. It is the opposite of a portable artifact, which is precisely the owner's
  stated reason for dropping it.
- `Module::deserialize_file` is **`unsafe` for a real reason**: wasmtime cannot validate precompiled code,
  so a tampered or stale cache file is arbitrary native code execution. wasmrt is
  `#![forbid(unsafe_code)]` in core and has **no deserialize path to attack**.
- **wasmrt is an interpreter, so there is no AOT artifact at all** — no cache directory, no invalidation
  logic, no version-tied file, no `unsafe`. It runs the portable `.wasm` on every platform identically.
  The whole `precompile → cache → mtime-check → deserialize` apparatus (≈50 lines of rsxtk) simply
  disappears.
- ⚠️ **The honest cost, which wasmrt must own:** without a JIT, sustained hot loops are slower — that
  tradeoff is accepted in `vision.md` ("win short-lived / native-FFI, not hot loops"). But once `.cwasm`
  is not the default, **wasmtime pays its compile cost on every run too**, which is exactly the
  cold-start regime wasmrt is built to win (4.48 ms at 48 KB). **This should be measured, not asserted**
  — rsxtk-on-wasmtime vs rsxtk-on-wasmrt, same machine, same modules, cwasm disabled. Logged for T11.

⚠️ **Method note:** this section replaced one that said rsxtk "has not been inspected — its requirements
are unknown", written minutes earlier because `../rsxtk` was not a sibling directory. Reading the actual
463 lines took one command and produced a dead feature flag, two non-blockers correctly identified as
non-scope, and a measurable competitive argument. *`best-practices.md` §2.3 — run it, don't recall it —
applies to consumers as much as to reference implementations.*

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
- **✅ SHIPPED at T8 / v0.9.0 (2026-08-06): `crates/wasmrt-capi/include/wasmrt.h`**, ~74 functions. That
  is now the authority; `docs/port/wasmrt.h.draft` is a **historical artifact** — do not read it as the
  current shape. **Four things in the draft did not survive contact with the code**: its per-proposal
  config toggles (core had no gating at all, so they would have been silent no-ops — gating was built
  for real), `wasmrt_linker_t` (core resolved imports *positionally*; a name-based `Linker` now lives in
  core), a store-attached WASI config (WASI is per-module), and `wasmrt_trap_message` promising
  "+ backtrace text" (there are none — the frame API ships its shape but reports 0 until T9). The
  draft's `wasmrt_config_set_tail_call` was **dropped outright**: that proposal is unimplemented, so the
  toggle would have gated nothing while reading as a security control.
- **What the loaders get.** Everything the ~38-fn survey called for: engine/store/linker, host functions
  with a **caller** handle (`wasmrt_caller_read`/`_write`), `define_wasi`,
  `define_unknown_imports_as_traps`, reactor `_initialize`, call-by-name with typed args, **raw
  `wasmrt_memory_data` + size** for hand-rolled Canonical ABI marshalling, exported-global reads, and
  traps/errors with messages. Plus two the survey did not ask for and embedders will want: **restricting
  which proposals a guest may use**, and **capping its memory / call depth / GC objects**.
- **One caveat to carry into the loader ports:** `wasmrt_caller_get_memory` always returns `false`. A
  durable memory handle must be tagged against a live store, and during a callback the store is
  mid-borrow. Use `wasmrt_caller_read`/`_write`/`_memory_size` inside callbacks — which is what the
  loaders actually need; the handle form exists so the wasmtime-shaped call sequence compiles.
- **A second, smaller caveat (2026-08-08):** **imported memories now link, but only from Rust.**
  `Linker::define_memory` and `Imports::with_instance_memory` are core-only — the C ABI has no
  `wasmrt_linker_define_memory`, because no surveyed loader needs one (they instantiate one module per
  store, with the guest *defining* its memory and the host reading it via `wasmrt_memory_data`). Adding it
  is additive when a loader does need it; `abi_version()` would stay 1. **Imported *tables* are refused in
  both layers** and that refusal is deliberate, not a gap — see `known-issues.md`.
- **What did change for embedders (2026-08-08):** imports are now **type-checked at link time**, so a
  wasm→wasm import whose signature or global type disagrees with the declaration fails to link instead of
  running. Host callbacks are unaffected — a `HostFunc` carries no declared signature, so the C ABI's
  `wasmrt_linker_define_func` is still trusted to match what the guest declared.

Three substrates today; **all 10 loaders are eventual targets, phased by effort:**

| Phase | Loaders | Mechanism |
|---|---|---|
| 1 | **c, v, zig** + **rs** | `wasmrt.h` C ABI (v/zig via the C loader) + a native `wasmrt` Rust crate for rs (free — wasmrt is Rust). |
| 2 | **dotnet, py** | thin bindings over `wasmrt.h` (replace the Wasmtime-NuGet / wasmtime-py packages). |
| 3 | **go** (was wazero), **jvm** (was Chicory) | FFI `wasmrt.h` via cgo / JNI-or-Panama. |
| 4 | **js, dart** | run guests through **wasmrt compiled to wasm32** (wasm-in-wasm) — the embed-inside-a-wasm-host vision; heaviest, last. |
