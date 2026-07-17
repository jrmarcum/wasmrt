# wazmrt build + docs + licensing — Port Map

Ground truth: live repo `…/wasmExamples/wazmrt`; target `…/wasmExamples/wasmrt` (empty: LICENSE+README only).
Rename wazmrt→wasmrt throughout; keep "Jon Marcum", year 2026. Pipeline: decode→validate→instantiate→execute + text front-end + WASI.

## 1. BUILD GRAPH (build.zig) — dual-target contract
One core module mod=addModule("wazmrt", src/root.zig) (dep-free, libc-free) → multiple artifacts:
- default `zig build`: CLI exe (src/main.zig imports mod), host target, user optimize. installArtifact + run step.
- default: C-ABI STATIC lib (src/wasm_c_api.zig, OWN root module, NO mod import), host, user optimize, smp_allocator.
  installHeader wasm.h + wazmrt.h. .static. NEVER cabi.linkLibC().
- `dll`: shared C-ABI lib (wasm_c_api.zig), linkage=.dynamic → zig-out/bin/wazmrt.dll, libc-free.
- `ffi-demo`: depends on dll; deno run --allow-ffi --allow-env examples/deno_ffi.mjs, env WAZMRT_DLL=...dll.
- `wasm`: freestanding (src/wasm_entry.zig), wasm32-freestanding, FORCED ReleaseSmall, wasm_allocator, entry=.disabled, rdynamic=true.
- `c-smoke`: tests/c_smoke.c + tests/c_abi_symbols.c, target x86_64-windows-GNU (.abi=.gnu). Separate wazmrt_csmoke static lib on gnu target; C module link_libc=true, -DLIBWASM_STATIC, includes include + third_party/wasm-c-api/include. mingw libc so C client links libc without MSVC; wazmrt lib stays libc-free. c_abi_symbols.c = link gate (all 319).
- `bench`: bench/bench.zig (fresh ReleaseFast mod), host, FORCED ReleaseFast, smp_allocator. bench -- out.wasm writes module; bench hash <file> measures SHA-256/Ed25519/decode/instantiate.
- `wasi-gate`: compiles real wasm32-wasi guests, runs via CLI exe, asserts exact stdout (expectStdOutEqual).
- `test`: mod_tests=addTest(mod) AND cabi_tests=addTest(wasm_c_api.zig), host, testing.allocator (comptime under test).
wasi-gate guests: Zig examples/hello_compiled.zig (wasm32-wasi ReleaseSmall, always) → "Hello from a compiled WASI program!\nbulk-memory memcpy works\nsaturating truncation works\n". C examples/c_hello.c (zig cc -target wasm32-wasi -Oz, always) → "Hello from C on wazmrt!\nsum 1..100 = 5050\n". Rust examples/rust_hello.rs (rustc --target wasm32-wasip1 -O, opt-in -Drust-gate=true) → "Hello from Rust on wazmrt!\nsum of squares 1..5 = 55\n".
Flags: -Drust-gate (bool default false); -Dtarget/-Doptimize. -Dlibc does NOT exist (hypothetical future). Distributed lib/dll/wasm meant ReleaseSmall (static -88% 1015→123KB, dll -58% 311→130KB) but build.zig uses user optimize for lib/dll — ship small = pass -Doptimize=ReleaseSmall. Only wasm hard-coded small, only bench hard-coded fast.
Rust port must reproduce: (a) native CLI, (b) C-ABI static+cdylib exposing standard wasm-c-api (crate-type=["staticlib","cdylib"] + #[no_mangle] extern "C" + generated wasm.h/wazmrt.h), (c) wasm32-unknown-unknown/freestanding build of runtime, (d) same gates (c-smoke + wasi-gate + bench).

## 2. build.zig.zon
.name=.wazmrt (→ package wasmrt). .version="0.1.0" (single-sourced w/ root.version; C ABI returns root.version.ptr). .minimum_zig_version="0.16.0". .fingerprint=0xd5b74fd2c2671c7 (regenerate only on fork; no Rust analogue). .dependencies=.{} ZERO external deps (only wasm.h vendored, not fetched — stay dep-light). .paths ship: build.zig, build.zig.zon, src, include, LICENSE-MIT, LICENSE-APACHE, NOTICE, README.md.

## 3. DESIGN DECISIONS — invariants NOT to "improve"
Interpreter = Option A (switch over pre-decoded IR), chosen over B (register machine/wasmi) and C (tail-call threading/wasm3). Interpreter NOT JIT/AOT (native codegen violates smallest-binary + can't run on wasm32-freestanding self-compile). Sub-rules: ONE shared opcode table (opcode.zig) for validate+IR+execute+assembler-reverse (never drift); untyped u64 stack slots; keep IR clean seam for later Option B. Perf ladder A→A.5(superinstructions)→B→JIT(native-only). Decide A→B with benchmarks not now.
Libc-free core: root.zig+deps link NO libc → native + wasm32-freestanding. C-ABI lib smp_allocator; wasm entry wasm_allocator; NEVER c_allocator. build.zig must not linkLibC(). Windows: libc without MSVC → "error: Unexpected". Rust: keep core #![no_std]-compatible / freestanding-wasm32 clean; C-ABI crate picks own global allocator.
Instance.recordTrap noinline — PERF LOAD-BEARING (~14% steady, 224→288 Mops/s, measured twice): errdefer expands at every try in ~200-arm switch; inlining evicts loop from i-cache. Rust: #[inline(never)] trap fn, cold error path.
Trap byte offsets resolved on demand (TrapFrame={func_index,pc} only); tracking cost ~7% cold-start + 4B/instr. Cold-start is the competed metric; traps rare.
Zero-copy decode: Reader borrows; Module stores section {id,offset,size} extents only. Decoded Module valid after input freed. EXCEPTION wasm_module_new keeps a binary copy (for serialize). Rust: Module<'a> borrowing or owned-arena; C-ABI path needs owned copy.
SIX C-ABI memory-safety invariants (wasm_c_api.zig = ONLY file handing raw ownership across C boundary; mistake = heap-corruption primitive):
 1. Every Ref free via refDelete/destroyRef — never free Ref directly.
 2. Nothing aliases a Ref without a handle (copy retains or dups export handle).
 3. A Ref naming an Instance owns a handle (refRetainInstance).
 4. Construct ref-able objects with whole-struct literal (field-by-field on alloc.create → garbage refcount).
 5. Instance owns a handle on its Module (embedder may delete module right after instance_new).
 6. Every wasm_X_delete for refcounted type calls release first (copy bumps; unconditional free double-frees on copy). Audit all 8 deleters when adding a ref-able type.
Enforced by tests not vigilance: cabi_tests under testing.allocator + lifecycle fuzz (#22, 400 seeds×250 ops, coverage-guided --fuzz). c_smoke.c CANNOT substitute (real allocator df→silent corruption→OK). Rust: Rc/Arc or manual refcount; Miri/ASAN under lifecycle fuzz. HIGHEST-RISK file.
Integration ABI IS the standard wasm-c-api (vendored wasm.h); wazmrt.h thin extension. Opaque struct wasm_*_t* handles. All 319 defined (undefined = link/dlopen break). wasm_X_copy refcounts for ref-able, deep-clones for type-objects. Bump wazmrt_abi_version on extension break.
Proposal scope (browser-standard bar): DONE — MVP, ref types, multi-table, bulk table/memory, extended-const, function-references, full WasmGC (i31/struct/array/casts/subtyping/rec/concrete refs), sat-trunc, WASI p1 + sandboxed FS. PLANNED — exception handling exnref form (try_table/throw/throw_ref + tag section 13), Phase 6 next. DEFERRED until browser-standard — WASI p2/p3 (component model), multi-memory, SIMD, legacy try/catch.

## 4. TESTING — definition of done
Counting: `zig build test --summary all` prints 254 (250 pass/4 skip) but 132 DISTINCT (122 core + 10 C-ABI); cabi_tests root imports root.zig → double count. QUOTE 132 not 254. 2 skip on unprivileged Windows.
Gates: Spec testsuite via .wast — wazmrt <file.wast> runs official testsuite (257 .wast in sibling wasmtk/tests/module/wasm_wast/testsuite-main, OUTSIDE repo, on removable media). Runs assert_return/trap/exhaustion + assert_invalid/malformed + register + invoke-by-module-name. Rep passing: i32 459/0, i64 415/0, call_indirect 169/0, func 171/0, table_init 729/0, table_copy 1649/0, block 222/0, if 240/0, f32/f64 2498/2 each (2 NaN-payload edge). assert_trap gated on isRuntimeTrap. GC gated by in-repo unit tests + hand .wast (official GC corpus not in tree).
.test.json corpus (wasm_mod 12 modules): {"<export>":[{args,expected,desc}]}; run+compare. All pass.
wasi-gate: compiles real wasm32-wasi (Zig+C always, Rust opt-in), asserts exact stdout; regression fails BUILD. Verified can fail (wrong expected→exit 1).
c-smoke: c_abi_symbols.c addresses all 319 (regenerate from PREPROCESSED header after vendoring); c_smoke.c behavior + trap surface (walks trap_origin/trace/frame_*, asserts offset lands on real unreachable byte).
Bench: two regimes — cold (decode+instantiate+one call) and steady (hot loop). Thesis (confirmed): native wins cold-start (2.4× trivial, 1.5× sum(1e6) vs Deno/V8), cedes hot loops to JIT (~264 Mops/s). Quote real-script cold ~4.4ms (46KB guest), NOT 0.8µs toy. Verify cost negligible: SHA-256 21µs (0.5%), Ed25519 105µs (2.4%).
Parity technique: decode/validation/.wast snapshots diffed vs same-session baseline (git stash A/B/A; never cross-day Mops/s, ~8% spread). Audits diff OUTPUT (N passed/failed) not exit codes.
DoD per slice ("wasmtk way"): small tested parts; each slice + unit tests + hand .wast (or corpus gate) + zero regressions across test/build/wasm (+ c-smoke where relevant).

## 5. SECURITY MODEL
Two orthogonal, both required: Authority (preopens+rights: BUILT) + Authenticity (signatures/pin: mostly DESIGN-ONLY). Signed ≠ harmless.
Sandbox (BUILT): guest can't exec (no proc_exec/spawn/fork in WASI p1; proc_exit self); can't reach non-WASI import (unknown→NOTSUP stub, other module import→HostTrap); preopen = entire FS authority (no --dir = zero files); rights ONLY narrow (path_open ∩ dir fd inheriting → --ro-dir propagates by construction); symlink containment. walkFull = RESOLVE_BENEATH userspace (stack of open dir handles, bottom=preopen never popped, one component no-follow through held handle TOCTOU-safe, symlinks followed via same loop, .. never below preopen, absolute→re-base to preopen root, symlink_max→ELOOP). Security = construction not string-checking. Mandated adversarial fuzz (canary-outside-preopen oracle). PORTER's real spec — reproduce walkFull, not Zig-std workarounds (#18/#23 Zig-0.16-specific; Rust cap-std/openat2 differs, but "never hand full guest path; resolve through held handles" carries over).
Pin verify (BUILT Phase 5): pin.zig pure logic (SHA-256, plaintext content-addressed DB, # mode: off|warn|enforce, stricter, decide(policy,pinned,opt_out,tty), 7 tests). CLI wazmrt pin <file> [--db] + verifyGate. Decided (don't re-derive): DB root-owned read-only plaintext (integrity from OWNERSHIP not secrecy); pin at install w/ privilege = verified install NOT TOFU; NO encryption (category error); NO machine-binding (attacker IS user). TOCTOU closed: verifyGate hashes in-memory buffer about to run, never re-read by path. Opt-out (--no-verify/--yes) only raises strictness, REFUSED under enforce. Default off. Invariant: integrity anchored by ownership or signature, never secrecy.
Open (signature path only, don't block built work): trust anchor (embed root PUBLIC key), signature format (custom section signed over all bytes except that section), default policy (deny-unsigned vs opt-in), revocation. Honest threat model: unprivileged→impossible via FS perms; root→nothing in software→detect (measured boot/TPM). Orchestrator invariants = advice not runtime-enforceable.

## 6. KNOWN-ISSUES status
#1-#16 (2026-07-09 audit ledger) DONE/cleared 2026-07-13 (host imports+register, validator tightening, start fn, assert_trap fidelity, externref/null_ref boxing, import-after-def rejection, (type $t) local-indexing, bulk table ops, spec-LEB128+custom-name+data-count).
Open residuals: #2/#6 (LOW) — instr-immediate valtype bytes (select_types, ref.null heaptype) still @enumFromInt unvalidated. #7 (LOW) — const-expr global.get allows any prior immutable global; spec restricts to imported. #17 residual TOCTOU (OPEN narrow) — path_open final-component only (stat no-follow then open with follow, Windows #18); Rust on cap-std/openat2 can close directly. #18 (Zig 0.16 std bug, WORKED AROUND) — openFile(.follow_symlinks=false) on Windows crashes host; Rust N/A. #23 (Zig 0.16 Windows Io gaps) — setTimestamps path-form @panic (routed via fd); hardLink unsupported (path_link DEFERRED ENOTSUP on Windows, works POSIX). Both Zig-std-specific.
DONE load-bearing: #19 trap backtraces, #20 all 319 + link gate, #21 four C-ABI mem-safety bugs + cabi_tests, #22 lifecycle fuzz (+2 bugs). Deliberately deferred C-ABI: shared-mutable imported globals; externref table slots via wasm_table_get (funcrefs only, reports null).

## 7. LICENSING — replicate verbatim (wazmrt→wasmrt)
License = dual MIT OR Apache-2.0 at consumer option. Files: LICENSE-MIT (2026 "Jon Marcum"), LICENSE-APACHE (canonical), NOTICE, third_party/LICENSES.md. Inbound=outbound. MIT/ISC flow into Apache-2.0 distribution. Verify upstream against actual LICENSE file not badge.
SPDX line verbatim: `SPDX-License-Identifier: MIT OR Apache-2.0`
SPDX header convention (accurate, don't overgeneralize): NOT on every file. On: NOTICE (line 11 indented), README.md, include/wazmrt.h. Zig sources use //! doc comments, NO per-file SPDX. Per third_party/LICENSES.md: per-file SPDX required ONLY on files containing third-party code; files adapting Apache-2.0 carry a change-note. Rust port: SPDX on C header(s), README, NOTICE, Cargo.toml (license="MIT OR Apache-2.0"); per-file on Rust sources optional (common/harmless, don't claim original required it).
C-header block (include/wazmrt.h → wasmrt.h) verbatim shape:
  /* wasmrt — extension header for the WebAssembly C API.
   * SPDX-License-Identifier: MIT OR Apache-2.0
   * Copyright (c) 2026 Jon Marcum
   * wasmrt's integration ABI IS the standard wasm-c-api (see <wasm.h>, vendored at third_party/wasm-c-api/include/wasm.h).
   * ... same pattern wasmtime uses with <wasmtime.h> alongside <wasm.h>. Include this; it pulls in <wasm.h>.
   * Static linking on Windows: ships a STATIC library, compile consumers with -DLIBWASM_STATIC ... */
Exports: uint32_t wasmrt_abi_version(void); const char *wasmrt_version_string(void);
NOTICE exact text (rename wazmrt→wasmrt):
  wazmrt
  Copyright (c) 2026 Jon Marcum
  This product is licensed under the terms of either:
    * the MIT license (see LICENSE-MIT), or
    * the Apache License, Version 2.0 (see LICENSE-APACHE),
  at your option.
      SPDX-License-Identifier: MIT OR Apache-2.0
  --- Third-party components ---
  Portions ... incorporate or adapt code from third-party projects. Each retained under its own license, documented in third_party/LICENSES.md.
  Where a component is Apache-2.0, this NOTICE (+ component's own NOTICE) is preserved/propagated per §4. Modifications carry prominent change notices per §4(b).
  Incorporated components:
    * WebAssembly C API — "wasm.h" from https://github.com/WebAssembly/wasm-c-api
      Copyright the wasm-c-api project authors. Licensed under Apache-2.0 (see third_party/wasm-c-api/LICENSE). Vendored verbatim as third_party/wasm-c-api/include/wasm.h.
third_party/LICENSES.md structure: obligations table (MIT/ISC/Apache-2.0/Apache-2.0 WITH LLVM-exception) + Adoption Checklist (benefit-vs-drawback, license confirmed vs upstream LICENSE, compatibility, copy upstream LICENSE/NOTICE into third_party/<component>/, ledger entry, change-notes+SPDX, update NOTICE if Apache) + Component Ledger + reference-project SPDX inventory.
Vendored wasm.h ledger entry (carry verbatim): source github.com/WebAssembly/wasm-c-api; commit 9d6b93764ac96cdd9db51081c363e09d2d488b4d (pinned); Apache-2.0; license file third_party/wasm-c-api/LICENSE; Modifications NONE (verbatim) → no change-notes, but top-level NOTICE MUST propagate (Apache §4). Impl in src/wasm_c_api.zig; extensions in include/wazmrt.h.
Change-note convention (for adapting Apache-2.0 code — none exist, runtime 100% original except vendored header):
  // Adapted from <project> (<SPDX>); modified by wazmrt — see third_party/LICENSES.md
  // SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
Trademark: no rights to Wasmtime/Wasmer/wazero names.
Reference-project inventory (all "Evaluating", NONE adopted — 100% original): wasm3 (MIT), WAMR (Apache-2.0 WITH LLVM-exc), wasmtime (Apache-2.0 WITH LLVM-exc), wasmer (MIT), wasmi (Apache-2.0 OR MIT), wazero (Apache-2.0), wain (MIT), wai (MIT), rust-wasm (ISC), wasm-c-api (Apache-2.0 ADOPTED). Copying any → Adoption Checklist + ledger entry.

## 8. examples/
deno_ffi.mjs (proves dll drop-in): Deno.dlopen wazmrt.dll, drives standard wasm-c-api by symbol (engine_new/store_new→byte_vec_new→module_new→instance_new→instance_exports→extern_as_func→func_call) on (func (export "answer")(result i32)(i32.const 42))→42. Real wasm_val_vec/extern_vec struct plumbing via DataView/UnsafePointer. Rust cdylib must pass this unchanged.
hello_wasi.wat: WASI p1 command, fd_write(1)+proc_exit(0)→"hello from wasi".
wasi-gate guests: hello_compiled.zig, c_hello.c, rust_hello.rs (exact stdout in build.zig).
wasi feature guests: wasi_files.zig (Phase 3 FS 16/16), wasi_clock_stdin.zig (Phase 2), wasi_leftovers.zig (4.3), wasi_symlink_traversal.zig (adversarial 5/5 Windows real symlinks).
.gitignore: .zig-cache/, zig-out/, *.o. Rust: target/, Cargo.lock policy.

BOTTOM LINE: reproduce multi-artifact contract (native CLI + static+cdylib C-ABI implementing standard wasm.h verbatim-vendored + freestanding-wasm build + c-smoke/wasi-gate/bench gates); preserve Option-A switch-over-IR + libc-free + shared-opcode-table; treat 6 C-ABI ownership rules, noinline trap path, secure-by-construction resolver, hash-the-bytes-you-run as invariants; carry dual MIT OR Apache-2.0 + NOTICE + Apache-2.0 wasm.h attribution (commit 9d6b9376…) verbatim, name changed.
