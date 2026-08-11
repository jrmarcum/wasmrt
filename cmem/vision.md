# Vision

`wasmrt` is a **blazingly-fast, smallest-binary** WebAssembly runtime, **itself compilable to `wasm32`**
so it can be embedded inside another wasm host, with a concrete purpose: **be the native engine the
`universalWasmLoader-*` projects run on, in place of wasmtime.** "Canonically similar to what wasmtime
can run, without being wasmtime."

## 🔒 What wasmrt is competing for (owner, 2026-08-11)

wasmrt began as a port of the Zig runtime `wazmrt` and was gated against it as a frozen oracle through
T9. **That relationship is over.** The two runtimes are now **independent entrants** for inclusion in
**wasmtk** and the **universalWasmLoader-\*** runtimes, and the contest is decided on **the smallest and
fastest binary**. `wazmrt` is under its own size/self-ownership program for the same contest, so
following its head would mean adopting a competitor's design choices — the opposite of what wins.

**`rsxtk` takes wasmrt by default**, through the native Rust interface: no C ABI, no cdylib, no FFI
marshalling. That makes **two distinct artifacts to optimize, with different levers**, and conflating
them will produce work that helps neither:

| consumer | artifact | what "small and fast" means there |
| --- | --- | --- |
| **rsxtk** (default, native) | `wasmrt-core` **rlib** | what LTO leaves in *rsxtk's* final binary — dead-code elimination across the crate boundary, generic bloat, and whether unused subsystems (WAT assembler, `.wast` runner, WASI) actually drop out. Calls are direct: **no FFI cost to shave**. |
| **wasmtk**, **universalWasmLoader-\*** | `wasmrt_capi` **cdylib** + `wasmrt.h` | the shipped **493.5 KiB** on disk, plus per-call FFI overhead at the C boundary. Nothing is dead-code-eliminated here — every exported symbol is reachable by definition. |

The practical consequence: a size win in the cdylib may be invisible to rsxtk, and an rlib win may not
show up in the cdylib at all. **Measure the artifact the consumer actually links.**

**Correctness anchors externally** now — the official spec testsuite, wasmtime's observable behaviour,
and the wasmtk WASI corpus. Those were always the harder test; the oracle was scaffolding.

## Success = three measurable axes (owner, 2026-07-17)

1. **Canonical** — runs the same WebAssembly **wasmtime** can run. Feature/spec conformance is the bar,
   not API-symbol parity. Scope = **full wasmtime browser-standard parity + memory64** (SIMD,
   multi-memory, threads/atomics, tail calls, GC, function-references, exceptions, **memory64** — added
   to scope 2026-07-27 once the frozen oracle had it), with **one caveat: WASI stays preview 1** —
   preview 2/3 and the component model are OUT (non-browser-standard).
2. **Fast** — beat wasmtk/Deno/V8. Preserve wazmrt's confirmed **cold-start win** (2.4× on a trivial
   call, 1.5× on `sum(1e6)` vs Deno/V8, which pays ~110 ms of V8 init + JIT + JS marshalling per run).
   wasmrt cedes sustained hot loops to a JIT — **accepted**; win short-lived / native-FFI, not hot loops.
3. **Small** — minimize **every** artifact (freestanding wasm, cdylib, static lib, CLI); aim smaller
   than the next-smallest runtime (wasm3 / WAMR), benchmarked later. See [design-decisions.md](design-decisions.md)
   for the size levers.

## Where the three axes actually stand (2026-08-07, post-T9 first pass)

Recorded because the axes above are the *destination* and are easy to read as a status claim.
**All three now carry a measurement** — that changed at T9; before it, two were assertions.

| Axis | Standing | Gap |
| --- | --- | --- |
| **Canonical** | **99.4%** of the official spec testsuite (62,113 / 385 / 2,163 over 284 files); the 533-file `.wat` corpus is clean through assemble→decode→validate, and every CLI command that takes a module accepts `.wat` directly; every proposal in the scope list runs **except tail calls** | **Tail calls are the one unimplemented scope item** — `return_call`/`return_call_indirect` are not in the opcode table (`return_call_ref` is, via function-references). Consequently the C ABI has **no tail-call feature flag**: a toggle that gates nothing is worse than none. **1.0 cannot be claimed without them.** |
| **Fast** | ✅ **MEASURED 2026-08-07.** Cold start **4.48 ms** for a 48 KB module (3.5 µs for a toy); steady state **~237 Mops/s** on a tight `loop`/`br_if`. Bench: `cargo run --release -p wasmrt-core --example bench`. | ⚠️ **A ~5% steady-state regression from the ninth T9 pass is still unattributed** and handed to T11 — two hypotheses were tested and rejected. That was a footnote while the oracle defined success; **under a contest decided on speed it is a live liability.** No cross-runtime comparison has been run on this machine. |
| **Small** | ✅ **MEASURED 2026-08-07.** CLI **621 KiB**, cdylib **493.5 KiB**, freestanding `wasm32` engine **158.1 KiB** (**137.5 KiB** after `wasm-opt -Oz`), engine + text toolchain **260.9 KiB**. Still **zero third-party dependencies**. | **The rlib-in-rsxtk figure has never been measured at all** — and that is the artifact the default consumer links. `wasm-opt -Oz` is worth ~13% and is **not wired into any build script**. No comparison against wasm3 / WAMR / wazmrt. |

**Honest summary:** *canonical* is the axis driven hardest and is nearly done. *Fast* and *small* have
real baselines — which is what **T11 (the optimization review) was blocked on**, so every proposal there
can now be stated as a delta.

⚠️ **What the anchor change makes urgent.** While `wazmrt` was the oracle, "canonical" was the gate and
the other two axes were aspirations with a footnote. **Under a contest decided on smallest-and-fastest,
fast and small are the gate**, and three gaps that were acceptable as footnotes are now the critical
path: (1) the unattributed ~5% steady regression, (2) **no measurement at all of what rsxtk actually
links**, and (3) no side-by-side against any competing runtime on one machine. All three are T11 work,
and T11 is no longer a late-stage nicety.

⚠️ **The rule that motivated the table stands, and now matters more: never quote another runtime's
numbers as wasmrt's.** Every figure above is wasmrt's own, measured here. A competitor's published
numbers are not a baseline — run both binaries on the same machine or say nothing.

## Why replace wasmtime under the loaders

The `universalWasmLoader-*` suite currently runs on wasmtime (and, per language, wazero/Chicory/host
`WebAssembly`). Standing them on wasmrt gives the owner: **no third-party runtime dependency**, a
**consistent** engine across languages, **licensing freedom**, and the **cold-start + size** wins above
— which matter precisely because the loaders run short-lived reactor/library modules. See
[loaders.md](loaders.md) for the integration and the phased target list. (Reference: wazmrt's own
`cmem/vision.md`.)
