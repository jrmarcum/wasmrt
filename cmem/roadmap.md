# Roadmap

## Status (2026-07-17) — PREP; port gate CLOSED

No Rust runtime code yet. The port begins only when the `wazmrt` oracle is frozen and `zig build test`
passes (run `scripts/check-wazmrt.sh`). wazmrt is **actively changing** — Phase 5 pin verification and
Phase 6 exception-handling **core** just landed (HEAD `5284154`); SIMD / multi-memory / threads / tail
calls are still **not** in wazmrt (they're the "oracle against wasmtime + spec testsuite" part of
wasmrt's scope — see `design-decisions.md`, `testing.md`).

**Prep DONE:**
- Scope reconciled (a faithful runtime port, not a canonical-ABI marshalling lib); fidelity =
  boundary-faithful + idiomatic Rust; success = **canonical / fast / small** (`vision.md`).
- **Full deep-read of wazmrt** — 6 subsystem maps (`docs/port/00`–`06`).
- **`universalWasmLoader` survey** — the `wasmrt.h` requirement, the ABI strategy (clean `wasmrt_*` +
  Rust crate), and the 10-loader phased target list (`docs/port/07`–`08`, `loaders.md`).
- **`wasmrt.h` v0 draft** (`docs/port/wasmrt.h.draft`) — held for review until wazmrt finalizes.
- **Oracle monitor** — `scripts/check-wazmrt.sh` + baseline.
- **Project scaffold** — `cmem/`, `bench/`, `examples/`, `tests/`, dual license files, `NOTICE`,
  `third_party/LICENSES.md`. Committed + pushed to `origin` (github.com/jrmarcum/wasmrt).

## Plan (once the gate opens)

1. **Finalize `wasmrt.h`** with the owner (naming, the store simplification, the `{id}`-handle model).
2. **Scaffold the crates** — `wasmrt-core` (no_std-friendly), `wasmrt-capi` (staticlib+cdylib),
   `wasmrt` CLI; dual-target build (wasm via build-std/`rust-lld`; native via `cargo-zigbuild`).
3. **Port bottom-up, parity-gated each step:**
   types + reader + opcode → module (decode) → validate → interp → text toolchain (sexpr/wat/wast) →
   wasi + CLI → **`wasmrt.h` C ABI** (redesign, not transliteration) → licensing/docs + all gates.
   Oracle Rust↔wazmrt for shared features.
4. **Extended proposals** (beyond current wazmrt): SIMD, multi-memory, threads/atomics, tail calls,
   exception handling — oracle against **wasmtime + the official spec testsuite**.
5. **Definition of done** — full Rust↔oracle parity on **both** targets (native + wasm), all gates
   green, size minimized.

## Loader integration phases (parallel track — see `loaders.md`)

- **Phase 1:** c, v, zig (`wasmrt.h`) + rs (native Rust crate) — primary early validation.
- **Phase 2:** dotnet, py (thin bindings over `wasmrt.h`).
- **Phase 3:** go (was wazero), jvm (was Chicory).
- **Phase 4:** js, dart (wasmrt-compiled-to-wasm, wasm-in-wasm).
