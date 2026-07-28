//! `module` — the decoded module + `decode`: all core sections, resolved import/export
//! extern types, function bodies, globals/memories/data; owned data model.
//!
//! Ported at **T3** (`cmem/roadmap.md`). **Invariants:** two-pass type-section decode
//! (pre-scan kinds) for rec-group forward references; reject reserved flag/valtype bytes;
//! parse the 64-bit limits flag (memory64, in scope). Custom-name + data-count checks.
