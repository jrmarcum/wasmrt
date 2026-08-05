//! `wasmrt` — the command-line interface.
//!
//! Grows across the roadmap (`cmem/roadmap.md`). Today it summarizes + type-checks a module,
//! calls an export, runs a WASI preview-1 program, assembles `.wat`, and runs `.wast` scripts.

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
        Some("wasi") => run_wasi_module(&args[2..]),
        Some("wat") => assemble_wat(&args[2..]),
        Some("wast") => run_wast(&args[2..]),
        Some(path) => summarize(path),
    }
}

/// One `--dir` / `--ro-dir` grant: the host directory, the name the guest sees, and
/// whether it is read-only.
struct Preopen {
    host: String,
    guest: String,
    read_only: bool,
}

/// Pull the leading `--dir` / `--ro-dir` flags off the command line, returning the preopens
/// as `(host, guest, read_only)` plus whatever is left.
///
/// `--dir <host>` maps the directory under its own name; `--dir <host>::<guest>` renames it
/// for the guest. `::` rather than `:` because a Windows host path starts `C:`.
///
/// Parsing stops at the first non-flag, so a guest argument that happens to look like
/// `--dir` is passed through to the guest rather than granting it access to anything.
fn take_dir_flags(args: &[String]) -> Result<(Vec<Preopen>, &[String]), String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let ro = match args[i].as_str() {
            "--dir" => false,
            "--ro-dir" => true,
            _ => break,
        };
        let Some(spec) = args.get(i + 1) else {
            return Err(format!("{} needs a directory", args[i]));
        };
        let (host, guest) = match spec.split_once("::") {
            Some((h, g)) => (h.to_string(), g.to_string()),
            None => (spec.clone(), spec.clone()),
        };
        out.push(Preopen { host, guest, read_only: ro });
        i += 2;
    }
    Ok((out, &args[i..]))
}

/// `wasmrt wasi <file.wasm> [args...]` — run a WASI preview-1 program's `_start`.
///
/// Exits with the guest's `proc_exit` code when it calls one, so shell pipelines see the
/// status the program intended.
fn run_wasi_module(rest: &[String]) -> ExitCode {
    // Preopens come first, so everything after the module path is the guest's own argv and
    // is never mistaken for a host flag.
    let (dirs, rest) = match take_dir_flags(rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(path) = rest.first() else {
        eprintln!(
            "wasmrt: usage: wasmrt wasi [--dir <host>[::<guest>]] [--ro-dir …] <file.wasm> [args...]"
        );
        return ExitCode::FAILURE;
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let md = match module::decode(&bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wasmrt: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = validate(&md) {
        eprintln!("wasmrt: {path}: invalid module: {e}");
        return ExitCode::FAILURE;
    }

    // A CSPRNG that cannot be seeded must fail loudly rather than run predictably.
    let Some(ctx) = wasmrt_core::wasi::WasiCtx::new() else {
        eprintln!("wasmrt: no OS entropy available; refusing to run with a predictable RNG");
        return ExitCode::FAILURE;
    };
    // argv[0] is the module path, then whatever follows on our command line.
    let mut ctx = ctx
        .with_args(std::iter::once(path.clone()).chain(rest[1..].iter().cloned()))
        .with_env(std::env::vars());
    // **The guest reaches nothing it was not explicitly granted.** With no `--dir`, every
    // path call returns BADF; there is no implicit cwd preopen.
    for p in &dirs {
        let rights = if p.read_only {
            wasmrt_core::wasi::fs::rights::READ_ONLY
        } else {
            wasmrt_core::wasi::fs::rights::ALL
        };
        if let Err(e) = ctx.preopen_dir(std::path::Path::new(&p.host), &p.guest, rights) {
            eprintln!("wasmrt: cannot preopen {}: errno {e}", p.host);
            return ExitCode::FAILURE;
        }
    }
    let shared = wasmrt_core::wasi::shared(ctx);

    let imports = match wasmrt_core::wasi::link(&md, &shared) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("wasmrt: {path}: cannot link WASI: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut inst = match Instance::new_with_imports(md, imports) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("wasmrt: {path}: instantiation failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let r = inst.invoke("_start", &[]);
    // `proc_exit` unwinds as a host trap, so consult the recorded code before treating the
    // error as a failure — an exit is a normal way for a WASI program to finish.
    if let Some(code) = shared.borrow().exit_code() {
        return ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1));
    }
    match r {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wasmrt: {path}: trap: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `wasmrt wat <file.wat> [-o out.wasm]` — assemble text to a binary.
fn assemble_wat(rest: &[String]) -> ExitCode {
    let Some(path) = rest.first() else {
        eprintln!("wasmrt: usage: wasmrt wat <file.wat> [-o <out.wasm>]");
        return ExitCode::FAILURE;
    };
    let src = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let bytes = match wasmrt_core::wat::assemble(&src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = rest
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| rest.get(i + 1));
    match out {
        Some(o) => {
            if let Err(e) = std::fs::write(o, &bytes) {
                eprintln!("wasmrt: cannot write {o}: {e}");
                return ExitCode::FAILURE;
            }
            println!("{o}: {} bytes", bytes.len());
        }
        None => println!("{path}: assembled {} bytes", bytes.len()),
    }
    ExitCode::SUCCESS
}

/// `wasmrt wast <file.wast | dir>...` — run spec scripts and report the pass profile.
fn run_wast(rest: &[String]) -> ExitCode {
    if rest.is_empty() {
        eprintln!("wasmrt: usage: wasmrt wast <file.wast | directory>...");
        return ExitCode::FAILURE;
    }
    let verbose = rest.iter().any(|a| a == "-v");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for a in rest.iter().filter(|a| !a.starts_with('-')) {
        let p = std::path::Path::new(a);
        if p.is_dir() {
            collect_wast(p, &mut files);
        } else {
            files.push(p.to_path_buf());
        }
    }
    files.sort();

    let (mut passed, mut failed, mut skipped, mut errored) = (0usize, 0usize, 0usize, 0usize);
    let mut worst: Vec<(String, usize)> = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read(f) else {
            eprintln!("wasmrt: cannot read {}", f.display());
            errored += 1;
            continue;
        };
        let name = f.file_name().unwrap_or_default().to_string_lossy();
        match wasmrt_core::wast::run_script(&src) {
            Ok(s) => {
                passed += s.passed;
                failed += s.failed;
                skipped += s.skipped;
                if s.failed > 0 {
                    worst.push((name.to_string(), s.failed));
                }
                if verbose || s.failed > 0 {
                    println!("{name}: {s}");
                    if verbose {
                        for m in s.failures.iter().take(3) {
                            println!("    {m}");
                        }
                    }
                }
            }
            Err(e) => {
                // The file did not even parse — a runner-level problem, kept separate from
                // assertion failures so it cannot hide in the totals.
                errored += 1;
                println!("{name}: PARSE ERROR: {e}");
            }
        }
    }

    worst.sort_by_key(|(_, c)| core::cmp::Reverse(*c));
    println!("\n=== conformance summary ===");
    println!("files      {} ({errored} unparseable)", files.len());
    println!("passed     {passed}");
    println!("failed     {failed}");
    println!("skipped    {skipped}  (constructs this build cannot put to the test)");
    let adjudicated = passed + failed;
    if adjudicated > 0 {
        let pct = (passed as f64) * 100.0 / (adjudicated as f64);
        println!("pass rate  {pct:.1}% of {adjudicated} adjudicated assertions");
    }
    if !worst.is_empty() {
        println!("\nworst files:");
        for (n, c) in worst.iter().take(15) {
            println!("  {c:>6}  {n}");
        }
    }
    ExitCode::SUCCESS
}

fn collect_wast(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_wast(&p, out);
        } else if p.extension().is_some_and(|x| x == "wast") {
            out.push(p);
        }
    }
}

fn print_help() {
    println!(
        "wasmrt {} — a fast, small WebAssembly runtime\n\n\
         USAGE:\n    \
         wasmrt <file.wasm>                     decode, summarize and type-check a module\n    \
         wasmrt run <file.wasm> <fn> [args...]  call an exported function\n    \
         wasmrt wasi [--dir D] <file.wasm> […]  run a WASI preview-1 program (_start)\n    \
         wasmrt wat <file.wat> [-o out.wasm]    assemble the text format to a binary\n    \
         wasmrt wast <file|dir>... [-v]         run .wast spec scripts\n    \
         wasmrt -h | --help                     show this help\n    \
         wasmrt -v | --version                  show the version\n\n\
         `wasi` covers stdio, args, environ, clocks, random, proc_exit and the sandboxed\n\
         filesystem. A guest reaches ONLY what you preopen:\n    \
           --dir <host>[::<guest>]     grant read-write access to a directory\n    \
           --ro-dir <host>[::<guest>]  grant read-only access (propagates to the subtree)\n\
         With no --dir, every path call returns BADF — there is no implicit cwd.",
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
