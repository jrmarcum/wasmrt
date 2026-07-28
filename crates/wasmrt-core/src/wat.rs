//! `wat` — the WAT text → wasm binary assembler, reusing the `opcode` table in reverse.
//!
//! Ported at **T6** (`cmem/roadmap.md`). Bar to hold: the frozen oracle's assembler has **no
//! gaps** — every construct across every proposal assembles. Prefer a hard `Err(UnknownInstr)`
//! over emitting wrong bytes (fall-through is the worst failure mode).
