//! `pin` — module pin verification: SHA-256 content-addressed allow-list + the pure
//! `decide()` policy matrix. Pure logic (no I/O), so it stays `std`-free.
//!
//! Ported alongside **T7** (`cmem/roadmap.md`). **Invariant:** hash the in-memory bytes you
//! are about to run (bytes-hashed == bytes-run); `enforce` denies before consulting the
//! opt-out; a malformed DB fails closed. See `cmem/security-model.md`.
