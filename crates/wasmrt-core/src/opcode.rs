//! `opcode` — the single shared opcode table + pre-decoded IR (`Op`/`Imm`/`Instr`) and
//! `decode_body`, feeding validate, interp, AND the assembler (in reverse). Define once.
//!
//! Ported at **T2** (`cmem/roadmap.md`). **Invariant:** `Op` discriminants are internal tags
//! ≠ wire bytes for 0xFC/0xFB-prefixed ops; the fc/gc reverse maps are the emit-side truth.
//! Keep values stable or the assembler breaks.
