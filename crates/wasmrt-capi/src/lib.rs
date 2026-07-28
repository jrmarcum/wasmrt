//! `wasmrt-capi` — the public **`wasmrt.h`** C ABI over `wasmrt-core`.
//!
//! **Designed, not transliterated, at T8** (`cmem/roadmap.md`): a clean lean `wasmrt_*`
//! surface (wasmtime-*shaped*, our names) built on lightweight `{id}` handles into a store
//! (NOT a wasm-c-api refcount object model) with caller-based host callbacks. The
//! `wasmrt.h` shape is finalized with the owner at the start of T8 — the decision-gate.
//!
//! This file is the T0 scaffold: a single smoke export that proves the `extern "C"` boundary
//! compiles and links as both a `staticlib` and a `cdylib`.
//!
//! **ABI rule (do not drift):** type pointer params as real pointers / `#[repr(C)]` structs
//! (`*const u32`, `*mut c_void`), never a hardcoded `i32` — lowers correctly on both wasm32
//! and 64-bit native. See `cmem/architecture.md`.

/// C ABI: the wasmrt ABI version. Mirrors wazmrt's `wazmrt_abi_version`.
///
/// Edition 2024 makes `no_mangle` an `unsafe(...)` attribute — it asserts the symbol name is
/// unique across the final link. `extern "C"` for the C calling convention.
#[unsafe(no_mangle)]
pub extern "C" fn wasmrt_abi_version() -> u32 {
    wasmrt_core::abi_version()
}

#[cfg(test)]
mod tests {
    #[test]
    fn abi_export_tracks_core() {
        assert_eq!(super::wasmrt_abi_version(), wasmrt_core::abi_version());
    }
}
