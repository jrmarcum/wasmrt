# wasmrt — project memory (CLAUDE.md)

Portable project memory for the **wasmrt** Rust WebAssembly runtime. Travels with the repo.
Detailed design notes: [`docs/port/`](docs/port/). Last updated 2026-07-17.

## Objective

Port the Zig runtime **`wazmrt`** (sibling repo `../wazmrt`) to Rust as **`wasmrt`** — a small, fast,
canonically-conformant WebAssembly runtime that **replaces wasmtime** as the engine beneath the
owner's `universalWasmLoader-*` projects. Boundary-faithful behavior, **idiomatic Rust internals**.

## ⛔ GATE — do not start the port yet

**Do not write runtime port code until `wazmrt` is complete and `zig build test` passes.** The
passing Zig build is the reference oracle. As of 2026-07-17 wazmrt is **actively changing** (Phase 6
exception-handling core just landed; `src/wat.zig` mid-edit). Run `scripts/check-wazmrt.sh` to see
current state / freeze-readiness. Until then: design + prep only.

## Success = three measurable axes (owner, 2026-07-17)

1. **Canonical** — runs the same WebAssembly **wasmtime** can run. Feature/spec conformance, not
   API-symbol parity. Scope = **full wasmtime browser-standard parity** (SIMD, multi-memory,
   threads/atomics, tail calls, GC, function-references, exceptions) with **one caveat: WASI stays
   preview 1** (p2/p3 and the component model are OUT — non-browser-standard).
2. **Fast** — beat wasmtk/Deno/V8. Preserve wazmrt's confirmed **cold-start win** (2.4× trivial /
   1.5× sum(1e6) vs Deno/V8); it cedes sustained hot loops to a JIT — accepted.
3. **Small** — minimize **every** artifact (freestanding wasm, cdylib, static lib, CLI); aim smaller
   than the next-smallest runtime (wasm3/WAMR), benchmarked later.

## Public API = `wasmrt.h` (NOT wasm-c-api, NOT wasmtime symbols)

The loaders are the consumers. They load a core `.wasm` + `.wit` sidecar and do the Component-Model
**Canonical ABI marshalling themselves** (`cabi_realloc`/`cabi_post_<name>`) — that is the LOADER's
job, not the runtime's. So `wasmrt.h` = the small **engine** surface the loaders call (the subset of
wasmtime's C API they use), in wasmtime's *shape* under `wasmrt_*` names.

- **ABI strategy (decided):** clean lean `wasmrt_*` C ABI **+ a native `wasmrt` Rust crate**. No
  exact-wasmtime-symbol compat shim.
- **Draft header:** [`docs/port/wasmrt.h.draft`](docs/port/wasmrt.h.draft) — ~40 fns. **On hold for
  review until wazmrt finalizes.** Load-bearing choice: **caller-based host callbacks**
  (`wasmrt_caller_get_memory`) so a host import can reach guest memory + return a value — the one
  real C-ABI capability wazmrt's current wasm-c-api lacks.
- Host surface needs NO tables/multi-value/memory.grow/threads/SIMD-calls/component API. (The engine
  still EXECUTES full-parity modules — a module may use SIMD internally; the host API just never
  exposes it.)

## Loader targets (all 10, phased — see [`docs/port/08`](docs/port/08-loader-survey-consolidated.md))

3 substrates today: wasmtime (c,dotnet,py,rs,v,zig), wazero (go), Chicory (jvm), host WebAssembly
(js,dart).
- **Phase 1:** c, v, zig (via `wasmrt.h`) + **rs (native `wasmrt` Rust crate — free, wasmrt is Rust)**.
- **Phase 2:** dotnet, py (thin bindings over `wasmrt.h`).
- **Phase 3:** go (replaces wazero), jvm (replaces Chicory) — FFI `wasmrt.h`.
- **Phase 4:** js, dart — run guests through **wasmrt compiled to wasm32** (wasm-in-wasm); heaviest.

## Proposed Rust layout

- **`wasmrt-core`** (`#![no_std]`-friendly, freestanding-wasm32 clean, no libc): `types` (ValType u32
  newtype), `reader`, `module`, `opcode` (shared IR table), `validate`, `interp`, `sexpr`, `wat`,
  `wast`, `wasi`, `pin`, `lib`. File structure mirrors wazmrt for oracle-diffing.
- **`wasmrt-capi`** (`crate-type=["staticlib","cdylib"]`): the `wasmrt.h` `#[no_mangle] extern "C"`
  surface over core. Ships `wasmrt.h`.
- **`wasmrt`** bin (CLI).
- Reproduce wazmrt's **dual-target contract**: native CLI + static + cdylib + freestanding-wasm32,
  plus gates (c-smoke-equiv under Miri, wasi-gate compiling real guests, bench cold vs steady).

## Port order (bottom-up, parity-gated each step)

1. types + reader + opcode → 2. module (decode) → 3. validate → 4. interp → 5. text toolchain
(sexpr/wat/wast) → 6. wasi + CLI → 7. **`wasmrt.h` C ABI (redesign, not transliteration)** →
8. licensing/docs + all gates + full Rust↔Zig parity = DONE.
**Oracle split:** features wazmrt has → diff Rust↔wazmrt; features wazmrt lacks (SIMD, multi-memory,
threads, tail calls; EH until stable) → oracle against **wasmtime + the official spec testsuite**.

## Load-bearing invariants (do NOT "improve" — see [`docs/port/00`](docs/port/00-synthesis.md))

- `ValType` = a **u32 newtype** with concrete refs bit-packed (bit31 concrete, bit30 nullable,
  bits28-29 family, bits0-27 index). Not a plain enum.
- Slot encoding: `null_ref = u64::MAX` checked BEFORE `i31_tag = 1<<63`; heap/func/extern small idx.
- `Op` discriminants are internal tags ≠ wire bytes for 0xFC/0xFB ops; `fc/gcSubOpcode` reverse maps
  are emit-side truth. One shared opcode table feeds validate + interp + assembler.
- Two-pass type-section decode (pre-scan kinds) for rec-group forward refs.
- LEB over-long/too-large rejection transcribed exactly.
- `Instance` retains its `Module` (UAF fix). Trap-record path `#[inline(never)]`/`#[cold]` (~14%
  i-cache regression otherwise). Trap byte offsets resolved lazily by re-decoding one body.
- Shared `Memory`/`Table` so `grow` is visible to importers.
- WASI sandbox secure BY CONSTRUCTION (`walkFull` handle-stack): never resolve a full guest path
  string; one component at a time through held handles; `..` never above preopen; absolute symlink
  targets re-base to preopen root. Rust may use cap-std/openat2 to close wazmrt's #17 residual TOCTOU.
- Pin verify hashes the in-memory bytes it runs; `enforce` denies before opt-out; DB parse fails closed.

## Licensing (replicate wazmrt verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026)

Dual **`MIT OR Apache-2.0`**. Carry `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE`, `third_party/LICENSES.md`.
SPDX header `SPDX-License-Identifier: MIT OR Apache-2.0` on the C header(s), README, NOTICE,
`Cargo.toml`. If the vendored wasm-c-api `wasm.h` is NOT reused (it isn't — we ship our own
`wasmrt.h`), that Apache-2.0 attribution may not be needed — confirm at scaffold time. Any adopted
third-party code triggers the Adoption Checklist + ledger entry. See
[`docs/port/06`](docs/port/06-build-docs-licensing.md).

## Open decisions (raise before/at scaffolding)

- `random_get`: keep wazmrt's non-crypto PRNG (parity) or upgrade to OS CSPRNG?
- Zero-dep vs allow cap-std/openat2 (closes #17 TOCTOU on the Rust side)?
- `wasmrt.h` review (naming, store simplification, id-handle model) — **held until wazmrt final**.
- core+capi crate split (recommended) vs single crate.

## Source oracle & tooling

- Oracle: `../wazmrt` (Zig 0.16). Build/test: `zig build`, `zig build test` (~132 distinct tests),
  `zig build wasi-gate`, `zig build wasm`, `zig build dll`, `zig build bench`.
- **`scripts/check-wazmrt.sh`** — reports wazmrt changes since baseline + runs `zig build test`;
  use it to judge freeze-readiness (the trigger to start porting).
