//! `wast` — the WAST script runner (`assert_return`/`assert_trap`/`assert_invalid`/
//! `assert_malformed`/`assert_unlinkable`/`register`), driving an `Instance`.
//!
//! Ported at **T6** (`cmem/roadmap.md`). Operates on script text (file I/O is the CLI's job,
//! so this stays `std`-free). Never count "we couldn't build it" as a pass — a harness that
//! treats its own gaps as success reports the shape of its gaps as conformance.
