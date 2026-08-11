//! **Every CLI path that loads a module accepts `.wat` text, not just `.wasm` binaries.**
//!
//! `wasmrt run prog.wat` used to fail with *"not a WebAssembly binary (bad magic)"* while the
//! assembler sat in the same executable, reachable only as a separate `wasmrt wat` step. The oracle
//! accepted `.wat` on its run path from the start, so this was a port/oracle divergence — the same
//! shape as the validation gap, just benign: a capability present in one and absent in the other,
//! with nothing comparing them.
//!
//! An integration test because the behaviour lives in the CLI's file loading, which no core-level
//! test reaches.

use std::process::Command;

fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

fn wasmrt(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
}

const WAT: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 42)))"#;

/// ⚠️ The regression this exists for.
#[test]
fn run_accepts_a_wat_file() {
    let p = write_temp("wasmrt_cli_run.wat", WAT);
    let out = wasmrt(&["run", p.to_str().unwrap(), "f"]);
    assert!(
        out.status.success(),
        "`wasmrt run prog.wat` must work; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}

/// The summarize path takes it too — all three module loaders share one helper, so they cannot
/// drift into accepting different things.
#[test]
fn summarize_accepts_a_wat_file() {
    let p = write_temp("wasmrt_cli_sum.wat", WAT);
    let out = wasmrt(&[p.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("validation OK"), "got: {stdout}");
}

/// Binaries must keep working — the sniff is an addition, not a replacement.
#[test]
fn run_still_accepts_a_wasm_binary() {
    let m = wasmrt_core::wat::assemble(WAT).expect("assemble");
    let p = write_temp("wasmrt_cli_run_bin.wasm", &m);
    let out = wasmrt(&["run", p.to_str().unwrap(), "f"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}

/// ⚠️ **Validation still runs, on the ASSEMBLED bytes.** Accepting text must not become a way to
/// skip the check — that would re-open, through a side door, the exact hole closed earlier today.
#[test]
fn an_ill_typed_wat_is_still_refused() {
    let p = write_temp(
        "wasmrt_cli_bad_type.wat",
        br#"(module (func (export "f") (result i32) i64.const 1))"#,
    );
    let out = wasmrt(&["run", p.to_str().unwrap(), "f"]);
    assert!(!out.status.success(), "an ill-typed .wat must be refused");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("invalid module"), "got: {err}");
    assert!(
        err.contains("type mismatch: expected i32, found i64"),
        "the wasmtime-shaped diagnostic must survive the text path; got: {err}"
    );
}

/// Dispatch is on the extension so the *stage* blamed stays honest: malformed text is an ASSEMBLE
/// failure, malformed bytes a DECODE failure. Sniffing content instead would feed a corrupt binary
/// to the assembler and report a syntax error for it.
#[test]
fn the_failing_stage_is_reported_accurately() {
    let bad_text = write_temp("wasmrt_cli_bad.wat", b"(module (func");
    let e = String::from_utf8_lossy(&wasmrt(&["run", bad_text.to_str().unwrap(), "f"]).stderr)
        .into_owned();
    assert!(e.contains("cannot assemble"), "malformed .wat: {e}");

    let bad_bin = write_temp("wasmrt_cli_bad2.wasm", b"not-wasm-at-all");
    let e = String::from_utf8_lossy(&wasmrt(&["run", bad_bin.to_str().unwrap(), "f"]).stderr)
        .into_owned();
    assert!(e.contains("decode failed"), "malformed .wasm: {e}");
}

/// The extension test is case-insensitive — `PROG.WAT` is still text.
#[test]
fn the_extension_match_ignores_case() {
    let p = write_temp("wasmrt_cli_upper.WAT", WAT);
    let out = wasmrt(&["run", p.to_str().unwrap(), "f"]);
    assert!(out.status.success(), "uppercase .WAT must also assemble");
}
