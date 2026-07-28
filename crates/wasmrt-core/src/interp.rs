//! `interp` — `Instance` + the switch interpreter over untyped `u64` slots (validation
//! proves types); GC heap; host imports; lazy trap backtrace.
//!
//! Ported in tested slices at **T5** (`cmem/roadmap.md`): int/float/control → memory/globals
//! → call/call_indirect → reftypes/tables → bulk → WasmGC → SIMD → multi-memory →
//! threads/atomics → memory64 → exception handling. **Invariants:** slot encoding checks
//! `null_ref = u64::MAX` **before** `i31_tag = 1<<63`; the trap-record path is
//! `#[cold]`/`#[inline(never)]` (inlining it into the dispatch loop is a measured i-cache
//! regression); `Instance` retains its `Module` (UAF fix); shared `Memory`/`Table` via
//! `Rc<RefCell<…>>` with a `Cell<u32>` refcount (single-thread ABI). See `cmem/design-decisions.md`.
