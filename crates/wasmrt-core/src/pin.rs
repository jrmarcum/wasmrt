//! `pin` — module pin verification: SHA-256 content-addressed allow-list + the pure
//! `decide()` policy matrix. Pure logic (no I/O), so it stays `std`-free.
//!
//! **STATUS: not implemented — this file is a stub.** wasmrt performs no authenticity check today.
//! Slated for T7, then T8, and slipped both; now **T9** (`cmem/roadmap.md`). **Invariant:** hash the in-memory bytes you
//! are about to run (bytes-hashed == bytes-run); `enforce` denies before consulting the
//! opt-out; a malformed DB fails closed. See `cmem/security-model.md`.
