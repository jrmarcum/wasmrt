# wasmrt Port — Synthesis Index

Deep read of wazmrt (Zig) completed 2026-07-17. Detail in sibling files:
- 01-c-abi.md — wasm_c_api.zig (the CONTRACT), 319 wasm.h fns, refcount object model, tests
- 02-decode-core.md — types/Reader/Module/opcode (ValType bit-packing, LEB, IR)
- 03-validate-interp.md — validate.zig + interp.zig (type-checker, switch interp, GC, traps)
- 04-text-toolchain.md — sexpr/wat/wast (assembler + spec-testsuite runner)
- 05-wasi-cli-pin.md — wasi.zig + main.zig + pin.zig (sandbox walkFull, CLI, pin verify)
- 06-build-docs-licensing.md — build graph, invariants, testing, security model, licensing verbatim

## Scale: ~14,573 lines src/*.zig, 132 distinct tests, dep-free, dual MIT OR Apache-2.0.

## Proposed Rust workspace (boundary-faithful, idiomatic; file structure mirrors wazmrt)
Two crates (or one crate multi-target):
- crate `wasmrt-core` (#![no_std]-friendly, freestanding-wasm32 clean, no libc):
  - types.rs (ValType u32 newtype + RefHeap), reader.rs, module.rs (decode), opcode.rs (Op/Imm/IR + shared table),
    validate.rs, interp.rs (Instance, switch interp), sexpr.rs, wat.rs, wast.rs, wasi.rs, pin.rs, lib.rs (root surface).
- crate `wasmrt-capi` (crate-type=["staticlib","cdylib"]): wasm_c_api.rs = #[no_mangle] extern "C" surface over core.
  - Generates the ~126 ref_*/*_vec_* fns via macro_rules! (exact symbol names).
  - Ships include/wasm.h (vendored verbatim, Apache-2.0, commit 9d6b9376…) + include/wasmrt.h (extension).
- bin `wasmrt` (CLI, main.rs) linking core.
- Build targets to reproduce: native CLI, static lib, cdylib (.so/.dylib/.dll), wasm32-unknown-unknown freestanding,
  + gates: c-smoke equiv (link all 319 + behavior + trap walk under Miri/ASAN), wasi-gate (compile real guests, assert stdout), bench (cold vs steady).

## Suggested port ORDER (bottom-up, each with parity gate before next)
1. types + reader + opcode (IR + LEB + ValType packing) — pure, heavily unit-tested; port the accept/reject LEB vectors first.
2. module (decode) — two-pass type section w/ forward refs; owned data model.
3. validate — spec Appendix algorithm; subtypeOf; const-expr.
4. interp — Instance (retains module), switch dispatch, slot encoding, branch(), trap machinery (#[inline(never)]), GC, host imports.
5. sexpr + wat + wast — assembler (reverse opcode table) + spec-testsuite runner; big parity lever (diff N passed/failed vs Zig).
6. wasi + main (CLI) — sandbox walkFull (reproduce construction, use cap-std/openat2 to close #17 residual), pin verify.
7. wasm_c_api — the contract; refcount object model, 6 mem-safety invariants, macro-generated surface, lifecycle fuzz under Miri. HIGHEST RISK — do last so runtime beneath is trusted, OR stub-first to unblock deno_ffi demo.
8. licensing files + README + CLAUDE.md; wire all gates; full Rust↔Zig output parity = DONE.

## Load-bearing invariants (easy to get wrong — DO NOT drift)
- ValType is a u32 NEWTYPE with concrete refs packed in high bits (bit31 concrete, bit30 nullable, bits28-29 family func/struct/array, bits0-27 index). NOT a plain enum.
- null_ref = u64::MAX, checked BEFORE i31_tag = 1<<63; heap/func/extern are small indices. Three-way slot discrimination depends on this order.
- Op enum discriminants are INTERNAL tags, NOT wire bytes for 0xFC/0xFB-prefixed ops; fc/gcSubOpcode reverse maps are the emit-side truth. Shared opcode table feeds validate+interp+assembler — define once.
- Two-pass type-section decode (pre-scan kinds) required for rec-group forward refs.
- LEB over-long/too-large rejection transcribed exactly (5th-byte >>4 + sign bits; 10th-byte v∈{0,0x7f}).
- Instance RETAINS its module (UAF fix); C-ABI: instance keeps module alive, export handle keeps instance alive.
- recordTrap / trap-record path #[inline(never)]/#[cold] — inlining = ~14% i-cache regression.
- Trap byte offsets resolved lazily by re-decoding one body — never stored per-instruction.
- Shared *Memory/*Table so grow is visible to importers (Rc<RefCell>/raw shared; growth reassigns backing slice in place).
- C-ABI: wasm_X_copy = REFCOUNT bump for ref-able objects (same holds), DEEP CLONE for type/value objects. 6 mem-safety invariants (01/06). Every delete calls release first. Test under leak/df-detecting allocator (Miri) + lifecycle fuzz — c_smoke on normal allocator can't catch UAF.
- Libc-free core; C-ABI lib never linkLibC(); allocators smp/wasm not c_allocator. Rust: core no_std-friendly; capi crate sets global allocator.
- WASI sandbox secure BY CONSTRUCTION (walkFull handle-stack) — never resolve a full guest path string against a dir API; resolve one component at a time through held handles. .. never rises above preopen; absolute symlink targets re-base to preopen root; symlink_max→ELOOP.
- Pin verify hashes the IN-MEMORY bytes it runs (bytes-hashed==bytes-run); enforce denies before opt-out; DB parse fails closed.
- Licensing: dual MIT OR Apache-2.0; NOTICE + wasm.h Apache-2.0 attribution (commit 9d6b9376…) carried verbatim, name wazmrt→wasmrt, "Jon Marcum" 2026.

## OPEN decisions to surface to owner before/at scaffolding
1. random_get uses a NON-crypto PRNG seeded from timestamp — keep as-is (parity) or upgrade to OS CSPRNG in Rust?
2. Vendor wasm.h verbatim (same pinned commit) vs regenerate — keep verbatim (it's the ABI contract + Apache attribution).
3. Rust deps policy — wazmrt is ZERO-dep. cap-std/openat2 for the sandbox would add deps but close the #17 TOCTOU. Allowed?
4. Freestanding wasm target: wasm32-unknown-unknown (no_std) vs wasm32-wasip1. wazmrt's freestanding export surface is tiny (abi_version + decode_section_count) — mirror or expand?
5. #17/#18/#23 are Zig-std-specific workarounds — Rust should implement the CLEAN version (atomic no-follow open). Confirm we may diverge from Zig here (behavior identical, mechanism better).
6. One multi-target crate vs core+capi split. Recommend split (core stays no_std/freestanding-clean).
