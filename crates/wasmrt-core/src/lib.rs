//! `wasmrt-core` — the idiomatic-Rust WebAssembly runtime core, a boundary-faithful
//! port of the Zig runtime `wazmrt` (see `cmem/` in the repo root).
//!
//! **Posture:** `#![no_std]` when the `std` feature is off, so the same source targets
//! native *and* a freestanding `wasm32` embed (libc-free). Heap use goes through `alloc`.
//! Only the OS-touching modules (WASI/sandbox) require `std`.
//!
//! **Pipeline:** decode → validate → instantiate → execute (a switch interpreter over a
//! pre-decoded IR — Option A, not a JIT). Text toolchain (sexpr/wat/wast) + WASI p1.
//!
//! The module tree below mirrors `wazmrt/src/` so each unit can be oracle-diffed against
//! its Zig counterpart. Modules are populated **bottom-up, parity-gated** per
//! `cmem/roadmap.md` (T1 types+reader → T2 opcode → T3 decode → T4 validate → T5 interp →
//! T6 text → T7 wasi). This file is the T0 scaffold: the crate compiles empty on every
//! target surface.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod types;
pub mod reader;
pub mod module;
pub mod opcode;
pub mod validate;
pub mod interp;
pub mod sexpr;
pub mod wat;
pub mod wast;
pub mod pin;

// WASI + the secure sandbox resolve real OS paths → they require `std`.
pub mod rng;
#[cfg(feature = "std")]
pub mod wasi;

/// The crate/runtime version string (`CARGO_PKG_VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The wasmrt C-ABI version — bumped on any breaking `wasmrt.h` change. Mirrors
/// wazmrt's `wazmrt_abi_version`. Starts at 0 while the ABI is pre-1.0 (finalized at T8).
#[must_use]
pub const fn abi_version() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn abi_version_is_zero_pre_release() {
        assert_eq!(super::abi_version(), 0);
    }

    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}
