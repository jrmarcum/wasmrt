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

## Why replace wasmtime under the loaders

The `universalWasmLoader-*` suite currently runs on wasmtime (and, per language, wazero/Chicory/host
`WebAssembly`). Standing them on wasmrt gives the owner: **no third-party runtime dependency**, a
**consistent** engine across languages, **licensing freedom**, and the **cold-start + size** wins above
— which matter precisely because the loaders run short-lived reactor/library modules. See
[loaders.md](loaders.md) for the integration and the phased target list. (Reference: wazmrt's own
`cmem/vision.md`.)
