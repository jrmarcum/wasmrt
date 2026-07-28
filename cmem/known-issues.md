# Known Issues

Issue tracker. The **gate is open** (2026-07-27) but **no wasmrt code exists yet** (port starts at T0 in
`roadmap.md`), so there are no wasmrt bugs — this records the **inherited concerns** from the frozen
wazmrt oracle that shape the port, plus the **open decisions** (now task-list gates). As the port
proceeds, log real wasmrt issues here (file:line + surfacing condition), mirroring wazmrt's ledger.

## Inherited from wazmrt — relevant to the port

- **#17 (final-component TOCTOU) + #18 (Zig std Windows crash) + #23 (Zig std Windows `Io` gaps)** —
  all **Zig-0.16-std-specific**. wazmrt's Windows `openFile(.follow_symlinks=false)` crashes the host,
  forcing a stat-then-open-with-follow that leaves a narrow final-component TOCTOU on `path_open`, and
  `setTimestamps`/`hardLink` path-forms are unavailable on Windows. **wasmrt should implement the CLEAN
  version** (atomic no-follow open via std / `cap-std` / `openat2(RESOLVE_BENEATH)`), closing #17's
  residual and dodging #18/#23 — same behavior, better mechanism. See `security-model.md`.
- **wasm-c-api refcount object model** — wazmrt's single highest-risk file (`wasm_c_api.zig`, 6
  memory-safety invariants, a lifecycle fuzz). wasmrt **avoids it** by designing `wasmrt.h` around
  lightweight `{id}` handles instead of refcounted objects (`design-decisions.md`). Still fuzz instance/
  memory lifetimes under Miri (`testing.md`).
- **wazmrt deferred C-ABI residuals** (shared-mutable imported globals; externref via `wasm_table_get`)
  — likely moot: none of the loaders need them (`docs/port/08`).

## Scope gap (not a bug — a work item) — SHRUNK TO ONE ITEM at the 2026-07-27 freeze

- **Only the tail-call proposal (`return_call`/`return_call_indirect`) has no wazmrt oracle.** wazmrt
  has `return_call_ref` (function-references) but not base tail calls. wasmrt's scope requires them
  (full browser-standard parity) → conform against **wasmtime + the official spec testsuite**.
- **Everything else wasmrt targets now HAS a wazmrt oracle.** SIMD (full 0xFD incl. relaxed),
  multi-memory, threads/atomics (0xFE), memory64, and exception handling (both exnref and legacy) all
  landed in wazmrt before the freeze — they moved from the wasmtime side of the oracle split to the
  wazmrt side. memory64 is **in scope** (owner, 2026-07-27). Re-check only if the frozen oracle drifts.

## Open decisions (owner — from `design-decisions.md`) — deferred as task-list GATES (2026-07-27)

The owner chose (2026-07-27) to **defer these as decision-gates at the relevant conversion step** rather
than resolve them up front. Decide each when the port reaches its task (see `roadmap.md`):

- `random_get`: parity PRNG vs OS CSPRNG → **WASI task** (wazmrt now uses a ChaCha CSPRNG, so parity ≈ CSPRNG).
- Zero-dep vs. `cap-std`/`openat2` to close #17 TOCTOU → **WASI-sandbox task**.
- `wasmrt.h` review — the "held until wazmrt finalizes" block is now **lifted** (oracle frozen) →
  **C-ABI task** (finalize with the owner before writing `wasmrt-capi`).
- core+capi crate split vs. single multi-target crate → **scaffold task**.

## Triggers (from `INDEX.md`)

Recheck the Zig-std items (#17/#18/#23) framing on every wazmrt sync — they may change as wazmrt or
Zig evolves, and they inform how much of the sandbox code is a straight port vs. a clean Rust rewrite.
