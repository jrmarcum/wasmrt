//! `validate` — the spec (§3 / Appendix) type-checker over the IR: value + control-frame
//! stacks, `subtype_of`, const-expr checks, per-memory index type (`mem_addr_ty`).
//!
//! Ported at **T4** (`cmem/roadmap.md`). Gate: `assert_invalid` / `assert_malformed` parity
//! vs the spec testsuite — no over-acceptance (both accepted AND still-rejected cases tested).
