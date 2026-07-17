# Known Issues

Prep-phase tracker. **No wasmrt code exists yet**, so there are no wasmrt bugs — this records the
**inherited concerns** from the wazmrt oracle that shape the port, plus the **open decisions**. As the
port proceeds, log real wasmrt issues here (file:line + surfacing condition), mirroring wazmrt's ledger.

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

## Scope gap (not a bug — a work item)

- **wazmrt lacks SIMD, multi-memory, threads/atomics, tail calls.** wasmrt's scope requires them (full
  wasmtime browser-standard parity). No wazmrt oracle exists for these → conform against **wasmtime +
  the official spec testsuite**. Exception handling (exnref) core landed in wazmrt 2026-07-17, so it
  *does* have an oracle once stable. Re-check at each wazmrt freeze.

## Open decisions (owner — from `design-decisions.md`)

- `random_get`: keep wazmrt's non-crypto PRNG (exact parity) or upgrade to an OS CSPRNG?
- Zero-dep (wazmrt has zero deps) vs. allow `cap-std`/`openat2` to close the #17 TOCTOU cleanly?
- `wasmrt.h` review — **held until wazmrt finalizes** (naming, store simplification, `{id}`-handle model).
- core+capi crate split (recommended) vs. single multi-target crate.

## Triggers (from `INDEX.md`)

Recheck the Zig-std items (#17/#18/#23) framing on every wazmrt sync — they may change as wazmrt or
Zig evolves, and they inform how much of the sandbox code is a straight port vs. a clean Rust rewrite.
