//! `types` — WebAssembly value/reference types and the section-id / decode-error vocabulary.
//!
//! Ported at **T1** (`cmem/roadmap.md`). **Invariant (do not drift):** `ValType` is a
//! `u32` newtype with concrete refs bit-packed (bit31 concrete, bit30 nullable, bits28-29
//! family, bits0-27 index) — NOT a plain enum; all accessors are pure bit ops. See
//! `cmem/design-decisions.md`.
