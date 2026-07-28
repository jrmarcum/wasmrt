//! `reader` — the zero-copy decode cursor: bounds-checked reads + spec-correct LEB128.
//!
//! Ported at **T1** (`cmem/roadmap.md`). **Invariant:** LEB128 over-long / integer-too-large
//! rejection transcribed exactly (5th-byte `>>4` + sign bits; 10th-byte `v ∈ {0,0x7f}`) —
//! conformance suites probe this. Borrows slices of the input; copies nothing.
