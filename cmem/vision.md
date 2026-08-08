# Vision

`wasmrt` carries `wazmrt`'s north star into Rust — a **blazingly-fast, smallest-binary** WebAssembly
runtime, **itself compilable to `wasm32`** so it can be embedded inside another wasm host — and adds a
concrete purpose: **be the native engine the `universalWasmLoader-*` projects run on, in place of
wasmtime.** "Canonically similar to what wasmtime can run, without being wasmtime."

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
| **Canonical** | **99.1%** of the official spec testsuite (61,712 / 578 / 2,469 over 284 files); every proposal in the scope list runs **except tail calls** | **Tail calls are the one unimplemented scope item** — `return_call`/`return_call_indirect` are not in the opcode table (`return_call_ref` is, via function-references). Consequently the C ABI has **no tail-call feature flag**: a toggle that gates nothing is worse than none. **1.0 = parity cannot be claimed without them.** |
| **Fast** | ✅ **MEASURED 2026-08-07.** Cold start **4.48 ms** for a 48 KB module (3.5 µs for a toy); steady state **~237 Mops/s** on a tight `loop`/`br_if`. Bench: `cargo run --release -p wasmrt-core --example bench`. | Against the oracle's recorded figures (~4.4 ms at 46 KB, ~264 Mops/s) that is **cold parity, ~90% steady** — but those were measured on a different machine, so it is a sanity check, not a result. The Deno/V8 comparison is **not** re-run for wasmrt; that claim is still inherited. |
| **Small** | ✅ **MEASURED 2026-08-07.** CLI **621 KiB**, cdylib **493.5 KiB**, freestanding `wasm32` engine **158.1 KiB** (**137.5 KiB** after `wasm-opt -Oz`), engine + text toolchain **260.9 KiB**. Still **zero third-party dependencies**. | The **wasm3 / WAMR comparison is not done** — it needs their binaries, not an estimate. `wasm-opt -Oz` is worth ~13% and is **not wired into any build script**. |

**Honest summary:** *canonical* remains the axis driven hardest. *Fast* and *small* now have real
baselines rather than inherited claims — which is what **T11 (the optimization review) was blocked on**;
every proposal there can now be stated as a delta. What is still missing on both is the **comparison to
another runtime**, which needs those runtimes present.

⚠️ **The rule that motivated all of this stands: never quote wazmrt's numbers as wasmrt's.** The table
above cites the oracle only alongside wasmrt's own measurement, and says so.

## Why replace wasmtime under the loaders

The `universalWasmLoader-*` suite currently runs on wasmtime (and, per language, wazero/Chicory/host
`WebAssembly`). Standing them on wasmrt gives the owner: **no third-party runtime dependency**, a
**consistent** engine across languages, **licensing freedom**, and the **cold-start + size** wins above
— which matter precisely because the loaders run short-lived reactor/library modules. See
[loaders.md](loaders.md) for the integration and the phased target list. (Reference: wazmrt's own
`cmem/vision.md`.)
