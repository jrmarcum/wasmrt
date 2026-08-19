//! `wasmrt` — the command-line interface.
//!
//! Grows across the roadmap (`cmem/roadmap.md`). Today it summarizes + type-checks a module,
//! calls an export, runs a WASI preview-1 program, assembles `.wat`, and runs `.wast` scripts.

// The CLI carries no `unsafe` either; `forbid` keeps it that way (`cmem/design-decisions.md`).
#![forbid(unsafe_code)]

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

/// Every flag this CLI recognises that takes a following value. Used to tell a **misplaced**
/// host flag from a guest argument that merely looks like one.
const HOST_FLAGS_WITH_VALUE: &[&str] = &["--dir", "--ro-dir"];
/// Every valueless flag this CLI recognises.
const HOST_FLAGS_BARE: &[&str] = &["--allow-symlink"];

/// Warn when a host flag appears where only the **guest** will ever see it.
///
/// ⚠️⚠️ **This is the fail-open half, and it is why the function exists.** Host flags are
/// recognised positionally; anything outside those positions is the guest's argv. For a
/// *preopen* a misplacement fails **closed** — the grant is simply never made, so the guest
/// gets less access than intended. **For a restriction flag it fails OPEN**: `--verify`,
/// `--pins` (T9e) and `--max-iterations` / `--max-*` (T9i) would be silently dropped, and the
/// user who asked for a bound would run without one and see no error.
///
/// **Warn, never refuse:** a guest may legitimately take `--dir` as its own argument, so
/// rejecting would break valid command lines. And **nothing after an explicit `--` is
/// examined** — that marker is the user saying "the rest is the guest's", and second-guessing
/// it would make `--` useless.
///
/// Matches wazmrt's behaviour (their H7); see `cmem/interop.md` §2.2.
fn warn_misplaced_host_flags(guest_argv: &[String]) {
    for a in guest_argv {
        if a == "--" {
            return; // everything past here is deliberately the guest's
        }
        if HOST_FLAGS_WITH_VALUE.contains(&a.as_str()) || HOST_FLAGS_BARE.contains(&a.as_str()) {
            eprintln!(
                "wasmrt: warning: `{a}` here is passed to the GUEST, not applied by wasmrt — \
                 host flags go before the module path or immediately after it \
                 (use `--` to pass it to the guest deliberately)"
            );
        }
    }
}

/// Pull the `--dir` / `--ro-dir` / `--allow-symlink` flags off one run of arguments,
/// returning the preopens plus whatever is left.
///
/// `--dir <host>` maps the directory under its own name; `--dir <host>::<guest>` renames it
/// for the guest. `::` rather than `:` because a Windows host path starts `C:`.
///
/// Parsing stops at the first non-flag. ⚠️ An **unrecognised** `--flag` in a leading position
/// is an **error**, not a module path: treating it as the path is how a typo becomes
/// "cannot read '--dir'" instead of a usable message, and how a misplaced restriction flag
/// disappears silently.
fn take_dir_flags(args: &[String]) -> Result<(Vec<Preopen>, bool, &[String]), String> {
    let mut out = Vec::new();
    let mut allow_symlink = false;
    let mut i = 0;
    while i < args.len() {
        // A valueless flag, so it is handled before the ones that consume an argument.
        if args[i] == "--allow-symlink" {
            allow_symlink = true;
            i += 1;
            continue;
        }
        // `--` ends host flags: everything after it belongs to the guest, verbatim.
        if args[i] == "--" {
            return Ok((out, allow_symlink, &args[i..]));
        }
        let ro = match args[i].as_str() {
            "--dir" => false,
            "--ro-dir" => true,
            // ⚠️ An unrecognised `--flag` is an ERROR, not the module path. Falling through
            // made `wasmrt wasi --typo x.wasm` report "cannot read '--typo'", and — the half
            // that matters — let a misplaced restriction flag vanish without a word.
            other if other.starts_with("--") => {
                return Err(format!("unknown option `{other}` (use `--` to pass it to the guest)"));
            }
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
    Ok((out, allow_symlink, &args[i..]))
}

/// `wasmrt wasi <file.wasm> [args...]` — run a WASI preview-1 program's `_start`.
///
/// Exits with the guest's `proc_exit` code when it calls one, so shell pipelines see the
/// status the program intended.
fn run_wasi_module(rest: &[String]) -> ExitCode {
    // Host flags are accepted in BOTH positions — before the module path (wasmrt's spelling)
    // and immediately after it (wazmrt's). 🔒 `cmem/interop.md` §2.2: a command line must do
    // the same thing under either runtime with only the program name changed, and flag
    // POSITION is part of an argument shape. Everything after the trailing run — or after an
    // explicit `--` — is the guest's argv.
    let (mut dirs, mut allow_symlink, rest) = match take_dir_flags(rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(path) = rest.first() else {
        eprintln!(
            "wasmrt: usage: wasmrt wasi [--dir <host>[::<guest>]] [--ro-dir …] [--allow-symlink] <file> [flags] [--] [args...]"
        );
        return ExitCode::FAILURE;
    };
    let path = path.clone();
    // The TRAILING run of host flags, immediately after the module path (wazmrt's spelling).
    let guest_argv: Vec<String> = match take_dir_flags(&rest[1..]) {
        Ok((more, sym, tail)) => {
            dirs.extend(more);
            allow_symlink |= sym;
            // ⚠️ Warn on the tail *including* any `--`, so the marker can stop the scan —
            // stripping it first made `wasmrt wasi m.wasm -- --dir X` warn about a flag the
            // user had explicitly handed to the guest, which is the one case that must stay
            // quiet. Caught by the case-5 probe, not by reading this.
            warn_misplaced_host_flags(tail);
            // Then drop the marker itself: the guest sees its arguments, not our separator.
            let tail = if tail.first().is_some_and(|a| a == "--") { &tail[1..] } else { tail };
            tail.to_vec()
        }
        Err(e) => {
            eprintln!("wasmrt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let path = path.as_str();
    let bytes = match read_module_bytes(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: {e}");
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
        eprintln!("wasmrt: {path}: {}", invalidity_report(&e));
        return ExitCode::FAILURE;
    }

    // A CSPRNG that cannot be seeded must fail loudly rather than run predictably.
    let Some(ctx) = wasmrt_core::wasi::WasiCtx::new() else {
        eprintln!("wasmrt: no OS entropy available; refusing to run with a predictable RNG");
        return ExitCode::FAILURE;
    };
    // argv[0] is the module path, then whatever follows on our command line.
    let mut ctx = ctx
        .with_args(std::iter::once(path.to_string()).chain(guest_argv.iter().cloned()))
        .with_env(std::env::vars());
    // **The guest reaches nothing it was not explicitly granted.** With no `--dir`, every
    // path call returns BADF; there is no implicit cwd preopen.
    for p in &dirs {
        // 🔒 Read-write does NOT include planting symlinks unless `--allow-symlink` asked for it:
        // a workload run has no need to create links, and denying it removes a guest-controlled
        // primitive a second process could later repoint. Following an EXISTING link is unaffected.
        let rights = if p.read_only {
            wasmrt_core::wasi::fs::rights::READ_ONLY
        } else if allow_symlink {
            wasmrt_core::wasi::fs::rights::ALL
        } else {
            wasmrt_core::wasi::fs::rights::READ_WRITE
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
            print_backtrace(&inst);
            ExitCode::FAILURE
        }
    }
}

/// Read a module file, **assembling it first when it is `.wat` text**.
///
/// `wasmrt run prog.wat` used to fail with *"not a WebAssembly binary (bad magic)"* — the assembler
/// was right there in the same binary, but only reachable as a separate `wasmrt wat` step. The oracle
/// accepted `.wat` on its run path all along, so this was a port/oracle divergence of exactly the
/// shape today keeps producing: a capability present in one and absent in the other, with nothing
/// comparing them.
///
/// **One helper, used by every path that loads a module** (`run`, `wasi`, summarize) rather than three
/// copies of the sniff — the same reasoning as wazmrt hanging its validation guard off the existing
/// `will_execute` predicate. A fourth loader added later inherits this instead of having to remember.
///
/// Dispatch is on the **extension**, matching the oracle: predictable, and it keeps a malformed
/// *binary* reporting a decode error rather than being fed to the assembler and blamed for bad syntax.
///
/// ⚠️ What executes is the **assembled bytes**, not the file on disk. That matters for anything that
/// hashes what it runs — `pin` is still a stub (T9e), but when it lands it must hash the assembled
/// module, exactly as wazmrt's `verifyGate` hashes the in-memory bytes rather than re-reading the path.
fn read_module_bytes(path: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    if std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wat"))
    {
        return wasmrt_core::wat::assemble(&bytes)
            .map_err(|e| format!("{path}: cannot assemble: {e}"));
    }
    Ok(bytes)
}

/// Format an invalid-module report, **shaped to match wasmtime**.
///
/// wasmtime 47 on `(func (result i32) i64.const 1)`:
///
/// ```text
/// Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64
/// ```
///
/// So: the byte offset **in decimal**, from the start of the module — the same origin wasmtime uses,
/// so the two tools' numbers are directly comparable on the same file — then the two types. The
/// function index is ours to add: wasmtime does not print it, and it is what makes a twenty-body
/// module tractable. Anything the validator did not record is simply omitted rather than guessed.
fn invalidity_report(e: &ValidateError) -> String {
    let site = wasmrt_core::validate::last_failure_site();
    let mut s = String::from("invalid module");
    if let Some(off) = site.offset {
        s.push_str(&format!(" at offset {off}"));
    }
    if let Some(i) = site.func_index {
        s.push_str(&format!(" (function {i})"));
    }
    match (site.expected, site.found) {
        // Match wasmtime's wording exactly where we have the same facts.
        (Some(exp), Some(found)) => s.push_str(&format!(": type mismatch: expected {exp:?}, found {found:?}")),
        _ => s.push_str(&format!(": {e}")),
    }
    s
}

/// Print the trap's call stack, innermost first, to **stderr**.
///
/// Stderr deliberately, like the trap line itself: `wasmrt wasi prog.wasm > out.txt` must capture
/// only the guest's output, not our diagnostics — the oracle mixes them into stdout and that is a
/// divergence we keep.
///
/// `+N` is the byte offset from the start of the module, which is what `wasm-objdump` prints.
fn print_backtrace(inst: &interp::Instance) {
    let frames = inst.backtrace();
    if frames.is_empty() {
        return;
    }
    let mut unnamed = false;
    for (i, f) in frames.iter().enumerate() {
        let lead = if i == 0 { "at" } else { "by" };
        match inst.frame_name(f).and_then(|n| core::str::from_utf8(n).ok()) {
            Some(name) => eprintln!("  {lead} {name} (fn[{}]) +{:#x}", f.func_index, f.offset),
            None => {
                unnamed = true;
                eprintln!("  {lead} fn[{}] +{:#x}", f.func_index, f.offset);
            }
        }
    }
    if unnamed {
        eprintln!("  (no name section: rebuild the guest unstripped for symbols)");
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
        // ⚠️⚠️ The **path**, not the basename. Seven basenames occur twice in the spec corpus
        // — `binary`, `br_on_cast`, `br_on_cast_fail`, `exports`, `imports`, `memory`, `throw`
        // — once at the top level and once under `proposals/`. Printing only the basename made
        // two different files indistinguishable in the report, and any analysis keyed on that
        // name silently merged them: a regression in one could be netted out by a gain in the
        // other and the per-file "no file lost a pass" check would report NONE.
        //
        // 🎓 **A gate whose identifier is not unique is not a gate.** Found 2026-08-19 when the
        // same file read 1 failure standalone and 12 in the corpus walk — which looked like a
        // harness state leak and was two files wearing one name.
        let name = f
            .strip_prefix(std::env::current_dir().unwrap_or_default())
            .unwrap_or(f)
            .to_string_lossy();
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
                        // All recorded failures, not a sample: triaging a file means seeing
                        // the distinct reasons, and three of forty-six is not a diagnosis.
                        for m in &s.failures {
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
         wasmrt <file>                          decode, summarize and type-check a module\n    \
         wasmrt run <file> <fn> [args...]       call an exported function\n    \
         wasmrt wasi [--dir D] <file> […]       run a WASI preview-1 program (_start)\n    \
         wasmrt wat <file.wat> [-o out.wasm]    assemble the text format to a binary\n    \
         wasmrt wast <file|dir>... [-v]         run .wast spec scripts\n    \
         wasmrt -h | --help                     show this help\n    \
         wasmrt -v | --version                  show the version\n\n\
         `wasi` covers stdio, args, environ, clocks, random, proc_exit and the sandboxed\n\
         filesystem. A guest reaches ONLY what you preopen:\n    \
           --dir <host>[::<guest>]     grant read-write access to a directory\n    \
           --ro-dir <host>[::<guest>]  grant read-only access (propagates to the subtree)\n    \
           --allow-symlink             let the guest CREATE symlinks (off by default)\n\n\
         With no --dir, every path call returns BADF — there is no implicit cwd.\n\n\
         <file> is a `.wasm` binary or `.wat` text; text is assembled first, then validated\n\
         and run exactly like a binary.",
        wasmrt_core::VERSION
    );
}

fn run_export(rest: &[String]) -> ExitCode {
    let (path, func) = match (rest.first(), rest.get(1)) {
        (Some(p), Some(f)) => (p.as_str(), f.as_str()),
        _ => {
            eprintln!("wasmrt: usage: wasmrt run <file> <function> [args...]");
            return ExitCode::FAILURE;
        }
    };
    let bytes = match read_module_bytes(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: {e}");
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
    // §4.5.1: instantiation is defined only for a **valid** module, so nothing executes before
    // validation. This path used to skip it — `wasmrt run` would happily execute an ill-typed
    // module and print a plausible answer, while `wasmrt wasi` next door refused the same bytes.
    // An asymmetry between two entry points of one binary is a bug, not a style difference.
    if let Err(e) = validate(&module) {
        eprintln!("wasmrt: {path}: {}", invalidity_report(&e));
        return ExitCode::FAILURE;
    }
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
            print_backtrace(&inst);
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
    let bytes = match read_module_bytes(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("wasmrt: {e}");
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
        // Name the function when the failure was inside one. A bare `TypeMismatch` for a module
        // with twenty bodies is a verdict without a diagnosis — localizing one by hand is what
        // T9a#9 cost before this existed.
        Err(e) => println!("  validation FAILED: {}", invalidity_report(&e)),
    }
}
