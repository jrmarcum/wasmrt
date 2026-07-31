//! `wasmrt` — the command-line interface.
//!
//! Grows across the roadmap (`cmem/roadmap.md`). Today (T3, v0.4.0) it summarizes a
//! decoded module; run/assemble/validate/WASI arrive in later stages.

use std::process::ExitCode;

use wasmrt_core::interp::{self, Instance, Value};
use wasmrt_core::module::{self, Extern, Module};
use wasmrt_core::types::ValType;
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
        Some("run") => run_export(&args[2..]),
        Some(path) => summarize(path),
    }
}

fn print_help() {
    println!(
        "wasmrt {} — a fast, small WebAssembly runtime\n\n\
         USAGE:\n    \
         wasmrt <file.wasm>              decode a module and print a summary\n    \
         wasmrt run <file.wasm> <fn> [args...]  run an exported function\n    \
         wasmrt -h | --help              show this help\n    \
         wasmrt -v | --version           show the version\n\n\
         `run` executes integer-compute functions today (float/memory/etc. arrive in later\n\
         releases). More (assemble / WASI) is on the roadmap.",
        wasmrt_core::VERSION
    );
}

fn run_export(rest: &[String]) -> ExitCode {
    let (path, func) = match (rest.first(), rest.get(1)) {
        (Some(p), Some(f)) => (p.as_str(), f.as_str()),
        _ => {
            eprintln!("wasmrt: usage: wasmrt run <file.wasm> <function> [args...]");
            return ExitCode::FAILURE;
        }
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let module = match module::decode(&bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wasmrt: {path}: decode failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Resolve the export's signature so args/results can be typed.
    let Some(sig) = module.exports.iter().find_map(|e| match &e.ty {
        Extern::Func(ft) if e.name == func => Some((e.index, ft.clone())),
        _ => None,
    }) else {
        eprintln!("wasmrt: no exported function `{func}` in {path}");
        return ExitCode::FAILURE;
    };
    let (index, ft) = sig;

    let arg_strs = &rest[2..];
    if arg_strs.len() != ft.params.len() {
        eprintln!(
            "wasmrt: `{func}` takes {} argument(s), got {}",
            ft.params.len(),
            arg_strs.len()
        );
        return ExitCode::FAILURE;
    }
    let mut args: Vec<Value> = Vec::with_capacity(arg_strs.len());
    for (s, &pt) in arg_strs.iter().zip(&ft.params) {
        match parse_arg(s, pt) {
            Ok(v) => args.push(v),
            Err(()) => {
                eprintln!("wasmrt: cannot parse `{s}` as {}", type_name(pt));
                return ExitCode::FAILURE;
            }
        }
    }

    let mut inst = match Instance::new(module) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("wasmrt: cannot instantiate {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match inst.invoke_index(index, &args) {
        Ok(results) => {
            let printed: Vec<String> = results
                .iter()
                .zip(&ft.results)
                .map(|(&v, &rt)| format_result(v, rt))
                .collect();
            println!("{}", printed.join(" "));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wasmrt: trap: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_arg(s: &str, ty: ValType) -> Result<Value, ()> {
    match ty {
        ValType::I32 => s.parse::<i32>().map(interp::i32_value).map_err(|_| ()),
        ValType::I64 => s.parse::<i64>().map(interp::i64_value).map_err(|_| ()),
        ValType::F32 => s.parse::<f32>().map(interp::f32_value).map_err(|_| ()),
        ValType::F64 => s.parse::<f64>().map(interp::f64_value).map_err(|_| ()),
        _ => Err(()),
    }
}

fn format_result(v: Value, ty: ValType) -> String {
    match ty {
        ValType::I32 => interp::as_i32(v).to_string(),
        ValType::I64 => interp::as_i64(v).to_string(),
        ValType::F32 => interp::as_f32(v).to_string(),
        ValType::F64 => interp::as_f64(v).to_string(),
        _ => format!("0x{v:x}"),
    }
}

fn type_name(ty: ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        _ => "a non-numeric type",
    }
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
