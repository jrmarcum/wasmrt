//! `wasmrt` — the command-line interface.
//!
//! Built out across **T3/T6/T7** (`cmem/roadmap.md`): summarize/run a module or export,
//! run `_start` (WASI), assemble `.wat`, run `.wast` scripts, and `pin`/`keygen`/`sign`.
//! This is the T0 scaffold: it prints the version handshake so the binary surface builds.

fn main() {
    println!(
        "wasmrt {} (abi {})",
        wasmrt_core::VERSION,
        wasmrt_core::abi_version()
    );
}
