//! `wasmrt` — the command-line interface.
//!
//! Grows across the roadmap (`cmem/roadmap.md`). Today (T3, v0.4.0) it summarizes a
//! decoded module; run/assemble/validate/WASI arrive in later stages.

use std::process::ExitCode;

use wasmrt_core::module::{self, Extern, Module};
use wasmrt_core::validate::{validate, ValidateError};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("-v" | "--version") => {
            println!(
                "wasmrt {} (abi {})",
                wasmrt_core::VERSION,
                wasmrt_core::abi_version()
            );
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(path) => summarize(path),
    }
}

fn print_help() {
    println!(
        "wasmrt {} — a fast, small WebAssembly runtime\n\n\
         USAGE:\n    \
         wasmrt <file.wasm>    decode a module and print a summary\n    \
         wasmrt -h | --help    show this help\n    \
         wasmrt -v | --version show the version\n\n\
         More (run / assemble / validate / WASI) arrives in later releases — see the roadmap.",
        wasmrt_core::VERSION
    );
}

fn summarize(path: &str) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match module::decode(&bytes) {
        Ok(m) => {
            print_summary(path, &m);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wasmrt: {path}: decode failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_summary(path: &str, m: &Module) {
    let defined_funcs = m.functions.len();
    let imported_funcs = m.imported_func_count() as usize;
    println!("{path}: WebAssembly module (version {})", m.version);
    println!("  sections   {}", m.sections.len());
    println!("  types      {}", m.comp_types.len());
    println!(
        "  functions  {} ({imported_funcs} imported + {defined_funcs} defined)",
        imported_funcs + defined_funcs
    );
    println!("  memories   {}", m.memories.len());
    println!("  tables     {}", m.tables.len());
    println!("  globals    {}", m.globals.len());
    println!("  imports    {}", m.imports.len());
    println!("  exports    {}", m.exports.len());
    println!("  data segs  {}", m.data.len());
    println!("  elem segs  {}", m.elements.len());
    if let Some(s) = m.start {
        println!("  start      func {s}");
    }
    if !m.exports.is_empty() {
        println!("  exported:");
        for e in &m.exports {
            let kind = match e.ty {
                Extern::Func(_) => "func",
                Extern::Table(_) => "table",
                Extern::Memory(_) => "memory",
                Extern::Global(_) => "global",
                Extern::Tag(_) => "tag",
            };
            println!("    {kind:<7} {}", e.name);
        }
    }
    match validate(m) {
        Ok(()) => println!("  validation OK"),
        // Deferred typing arm (SIMD / atomics / GC objects / EH) — not a verdict on the
        // module, just a gap in this release's validator.
        Err(ValidateError::UnsupportedValidation) => {
            println!("  validation SKIPPED (uses a construct the validator can't check yet)");
        }
        Err(e) => println!("  validation FAILED: {e}"),
    }
}
