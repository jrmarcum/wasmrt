# Roadmap

## Status (2026-07-27) — PORT phase; gate OPEN, oracle FROZEN

The conversion has **begun**. The `wazmrt` oracle reached full parity and is **frozen** at
`wazmrt@dadc727`; `zig build test` passes (489/493, 4 skip; Debug + ReleaseSafe green) — gate verified
2026-07-27. `scripts/check-wazmrt.sh` now watches for **oracle drift**, not freeze-readiness. The oracle
covers **every wasm proposal wasmrt targets except tail calls** (`return_call`/`return_call_indirect`),
so the oracle split has collapsed to that one item (see `design-decisions.md`, `testing.md`). **memory64
is in scope** (owner, 2026-07-27). **T0 (crate scaffold) is DONE; next is T1 (types + reader).**

**Prep DONE (pre-freeze):** scope reconciled (a faithful runtime port; fidelity = boundary-faithful +
idiomatic Rust; success = **canonical / fast / small**, `vision.md`); full **deep-read of wazmrt** (6
subsystem maps, `docs/port/00`–`06`); the **`universalWasmLoader` survey** + ABI strategy + 10-loader
phased list (`docs/port/07`–`08`, `loaders.md`); the **`wasmrt.h` v0 draft** (`docs/port/wasmrt.h.draft`,
review-block now lifted); the **oracle monitor**; and the **project scaffold** (`cmem/`, `bench/`,
`examples/`, `tests/`, dual licenses, `NOTICE`, `third_party/LICENSES.md`) — committed + pushed to
`origin`.

## Conversion task list (bottom-up, parity-gated each step)

Port order mirrors wazmrt's `src/` so every module can be oracle-diffed against its Zig counterpart.
Each task is DONE only when it adds unit tests **and** passes its parity/conformance gate with **zero
regressions across all build surfaces** (`cargo test` / native / `wasm32` / c-smoke where relevant) —
diff the OUTPUT counts, not exit codes (`testing.md`). `[ ]` = not started.

- **T0 — Crate scaffold + dual-target build. ✅ DONE 2026-07-27.** *Decision-gate resolved (owner):*
  **workspace of 3** — `crates/wasmrt-core` (`no_std`-friendly, `default=["std"]`; `wasi` behind `std`),
  `crates/wasmrt-capi` (`staticlib`+`cdylib`+`rlib`, ships `include/wasmrt.h`), `crates/wasmrt-cli` (bin
  `wasmrt`). Edition 2024, size-first `[profile.release]` (`opt-level="z"`+LTO+`codegen-units=1`+strip+
  `panic="abort"`), workspace clippy lints. core's module tree is stubbed to mirror wazmrt `src/` (each
  stub cites its port task + invariant). **Gate met:** all four surfaces build — CLI bin, `libwasmrt_capi.a`,
  `wasmrt_capi.dll`, and freestanding `wasm32-unknown-unknown` core (`--no-default-features`, no_std,
  libc-free); `cargo test` 3/0, `cargo clippy` clean. **Build-host note:** on Windows use the
  **`x86_64-pc-windows-gnullvm`** host (LLVM-MinGW + UCRT — matches the libc-free/no-MSVC ethos); plain
  `-gnu` fails to link (no classic `libgcc`). `rust-toolchain.toml` pins bare `nightly` (portable) — set
  the machine default-host to gnullvm. `cargo-zigbuild` + build-std tuning deferred to when cross-native/
  size builds are actually needed. `[x]`
- **T1 — `types` + `reader`.** `ValType` `u32` newtype (bit-packed concrete refs — invariant), `SectionId`,
  the `DecodeError` set; zero-copy `Reader` with spec-correct LEB128 (over-long / too-large rejection).
  Gate: port wazmrt's LEB accept/reject + ValType-bit-op vectors 1:1. **Highest-value first task after
  scaffold.** `[ ]`
- **T2 — `opcode` (the shared IR table).** `Op`/`Imm`/`Instr` + `decodeBody`; the `fc`/`gc` reverse maps
  (internal tags ≠ wire bytes — invariant). One shared table for validate + interp + assembler. Gate:
  decode-coverage snapshot == wazmrt for the `wasm_mod` corpus. `[ ]`
- **T3 — `module` (decode).** All core sections + resolved import/export extern types + bodies;
  two-pass type-section decode (rec-group forward refs); custom-name + data-count checks; reserved-byte
  rejection; 64-bit limits flag (memory64). Gate: `wasm_mod` 12/12 decode; malformed-binary rejection
  matches wazmrt. `[ ]`
- **T4 — `validate`.** Spec type-checker over the IR (value + control-frame stacks) + module-level checks
  (const-exprs, select/if/call_indirect/alignment/memory-presence, `memAddrTy` per-memory index type).
  Gate: `assert_invalid`/`assert_malformed` parity vs the spec testsuite; no over-acceptance. `[ ]`
- **T5 — `interp` (the switch interpreter).** Untyped `u64` slots; the slot-encoding order invariant
  (`null_ref` before `i31_tag`); `#[cold]`/`#[inline(never)]` trap path with lazy byte-offset resolve;
  shared `Memory`/`Table` (`Rc<RefCell>`, `Cell<u32>` refcount — single-thread ABI); `Instance` retains
  its `Module` (UAF fix). Build in tested slices mirroring wazmrt: int/float/control → memory/globals →
  call/call_indirect (multi-table) → reference types + table ops → bulk mem/table → **full WasmGC** →
  **SIMD (full 0xFD + relaxed)** → **multi-memory** → **threads/atomics (0xFE)** → **memory64** →
  **exception handling (exnref + legacy)**. Gate per slice: golden-vector parity Rust↔wazmrt + the
  relevant `.wast` files. `[ ]`
- **T6 — Text toolchain (`sexpr` → `wat` → `wast`).** S-expr parser → WAT→binary assembler (reuse the
  opcode table in reverse) → WAST script runner. Gate: run the official spec testsuite; match wazmrt's
  ~60k-assertion pass profile (the assembler has no gaps in the oracle — hold that bar). `[ ]`
- **T7 — WASI preview 1 + CLI.** Native host imports (stdio/args/environ/clocks/`poll_oneoff`/random/
  `proc_exit`) + the sandboxed filesystem. ⛔ *Decision-gates:* **`random_get`** (parity ≈ wazmrt's
  ChaCha CSPRNG) and **zero-dep vs `cap-std`/`openat2`** for the secure path resolver (`walkFull`
  equivalent; close wazmrt's #17 TOCTOU cleanly). Gate: **wasi-gate** compiles real Zig/C/Rust guests and
  matches stdout; run the wasmtk WASI corpus (400 runnable) to wazmrt's result. `[ ]`
- **T8 — `wasmrt.h` C ABI (redesign, not transliteration).** ⛔ *Decision-gate (block lifted):*
  **finalize `wasmrt.h`** with the owner first (naming, store simplification, `{id}`-handle model) —
  from `docs/port/wasmrt.h.draft`. Then implement `#[no_mangle] extern "C"` over core: lightweight `{id}`
  handles (no wasm-c-api refcount model), caller-based host callbacks, `#[repr(C)]` boundary structs, real
  pointers. Gate: `tests/c_smoke.c` (decode→instantiate→call→memory→global→trap) + `tests/abi_symbols.c`
  link-completeness + lifecycle fuzz under **Miri**. `[ ]`
- **T9 — Licensing, docs, size, and all gates green.** Add `LICENSE-MIT`/`LICENSE-APACHE`/`NOTICE`/
  `third_party/LICENSES.md` (name wazmrt→wasmrt, "Jon Marcum" 2026); minimize every artifact (`opt-level=z`
  + LTO + `codegen-units=1` + strip + `wasm-opt -Oz`); cold-vs-steady bench. Gate: DoD below. `[ ]`

**Extended proposal — the one oracle-split residual:** **tail calls** (`return_call`/`return_call_indirect`)
have no wazmrt oracle → conform against **wasmtime + the official spec testsuite**. Slot this into T5/T6
alongside the rest; everything else parity-tests directly against the frozen wazmrt.

**Definition of done** — full Rust↔oracle parity on **both** targets (native + `wasm32`), all gates green
(spec testsuite, wasi-gate, c-smoke, Miri fuzz), size minimized, and the drift monitor clean (or the
oracle re-baselined deliberately).

## Loader integration phases (parallel track — see `loaders.md`)

- **Phase 1:** c, v, zig (`wasmrt.h`) + rs (native Rust crate) — primary early validation.
- **Phase 2:** dotnet, py (thin bindings over `wasmrt.h`).
- **Phase 3:** go (was wazero), jvm (was Chicory).
- **Phase 4:** js, dart (wasmrt-compiled-to-wasm, wasm-in-wasm).
