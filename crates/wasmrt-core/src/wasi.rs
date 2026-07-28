//! `wasi` — WASI preview 1 host imports + the secure sandbox (per-component handle-stack
//! path resolver, the `walkFull` equivalent). **Requires `std`** (real OS file APIs).
//!
//! Ported at **T7** (`cmem/roadmap.md`). ⛔ Decision-gates at this task: `random_get`
//! (parity ≈ wazmrt's ChaCha CSPRNG) and zero-dep vs `cap-std`/`openat2(RESOLVE_BENEATH)` to
//! close the #17 final-component TOCTOU cleanly. **Invariant:** secure BY CONSTRUCTION — never
//! resolve a full guest path string; resolve one component at a time through held handles;
//! `..` never rises above the preopen; absolute symlink targets re-base to the preopen root.
//! See `cmem/security-model.md`.
