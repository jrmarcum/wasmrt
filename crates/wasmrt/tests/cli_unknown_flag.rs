//! 🔒 **`interop.md` §2.4a — the UNKNOWN-FLAG rule (owner, 2026-09-19).** A flag-shaped argument in a
//! host-flag position that the command does not recognise is an error: stderr says `unknown flag` and
//! names it, and the exit status is 1. Guest positions — after `--`, or after the first non-flag
//! argument following the module path — are never examined.
//!
//! ⚠️ **A SINGLE-DASH token immediately after the module path is the GUEST's** (owner, 2026-09-19:
//! *"I want the first option, but I do not want the 'looks like' part"*). Every host flag that can
//! appear there is double-dash, so a single-dash token cannot be one, and `prog.wasm -la` needs no
//! `--`. Where no guest argv exists — before the path, or in `wast`/`wat`/summarize — nothing can own
//! it and it is an unknown flag. **No heuristic anywhere:** a token is judged by its POSITION and by
//! whether the command knows it, never by resembling a flag name.
//!
//! Measured before the rule, on BOTH runtimes: an unknown flag was silently ignored after the path
//! (summarize), anywhere in `wast`, after the file in `wat`, and misreported as "cannot read `--x`" in
//! front of a path. Every row below was one of those.

use std::process::{Command, Output};

fn write_temp(name: &str, bytes: &[u8]) -> String {
    let p = std::env::temp_dir().join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p.to_str().unwrap().to_string()
}

fn wasmrt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
}

fn assert_unknown_flag(args: &[&str], flag: &str) {
    let out = wasmrt(args);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{args:?} must exit 1; stderr: {err}");
    assert!(
        err.contains(&format!("unknown flag `{flag}`")),
        "{args:?} must say `unknown flag` and name {flag}; stderr: {err}"
    );
}

const MODULE: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 7)))"#;
const COMMAND: &[u8] = br#"(module (memory (export "memory") 1) (func (export "_start")))"#;
const SCRIPT: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 7)))
                          (assert_return (invoke "f") (i32.const 7))"#;

#[test]
fn a_flag_in_first_position_is_unknown_not_an_unreadable_file() {
    assert_unknown_flag(&["--bogus"], "--bogus");
    assert_unknown_flag(&["-x"], "-x");
}

#[test]
fn summarize_refuses_an_unknown_flag_on_either_side_of_the_path() {
    let m = write_temp("wasmrt_uf_sum.wat", MODULE);
    assert_unknown_flag(&["--bogus", &m], "--bogus");
    assert_unknown_flag(&[&m, "--bogus"], "--bogus");
    assert_eq!(wasmrt(&[&m]).status.code(), Some(0), "the plain form still works");
}

#[test]
fn run_refuses_a_flag_where_the_path_or_function_goes_but_not_in_its_arguments() {
    let m = write_temp("wasmrt_uf_run.wat", MODULE);
    assert_unknown_flag(&["run", "--bogus", &m, "f"], "--bogus");
    assert_unknown_flag(&["run", &m, "--bogus"], "--bogus");
    // `-1` is a VALUE in argument position, never a flag.
    let add = write_temp(
        "wasmrt_uf_add.wat",
        br#"(module (func (export "add") (param i32 i32) (result i32)
             (i32.add (local.get 0) (local.get 1))))"#,
    );
    let out = wasmrt(&["run", &add, "add", "-1", "2"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1");
}

#[test]
fn wasi_refuses_in_host_positions_and_leaves_guest_positions_alone() {
    let m = write_temp("wasmrt_uf_wasi.wat", COMMAND);
    assert_unknown_flag(&["wasi", "--bogus", &m], "--bogus");
    assert_unknown_flag(&["wasi", &m, "--bogus"], "--bogus"); // a `--flag` there could be ours
    // Guest positions: after the first guest argument, and after `--`.
    assert_eq!(wasmrt(&["wasi", &m, "arg", "--bogus"]).status.code(), Some(0));
    assert_eq!(wasmrt(&["wasi", &m, "--", "--bogus"]).status.code(), Some(0));
    // ⚠️ A SINGLE-DASH token right after the path is the guest's, with no `--` and no warning —
    // and that includes one that resembles a host flag (`-dir`), because there is no heuristic.
    assert_eq!(wasmrt(&["wasi", &m, "-la"]).status.code(), Some(0), "`-la` is the guest's");
    assert_eq!(wasmrt(&["wasi", &m, "-dir", "."]).status.code(), Some(0), "no `looks like` rule");
    // …but BEFORE the path there is no guest to own it.
    assert_unknown_flag(&["wasi", "-la", &m], "-la");
    // A real host flag in that position still applies.
    assert_eq!(wasmrt(&["wasi", &m, "--allow-symlink"]).status.code(), Some(0));
}

#[test]
fn wast_and_wat_have_no_guest_argv_so_every_position_is_checked() {
    let s = write_temp("wasmrt_uf.wast", SCRIPT);
    assert_unknown_flag(&["wast", "--bogus", &s], "--bogus");
    assert_unknown_flag(&["wast", &s, "--bogus"], "--bogus");
    assert_eq!(wasmrt(&["wast", &s, "-v"]).status.code(), Some(0), "`-v` is recognised");

    let w = write_temp("wasmrt_uf_wat.wat", MODULE);
    let out = std::env::temp_dir().join("wasmrt_uf_wat_out.wasm");
    assert_unknown_flag(&["wat", "--bogus", &w], "--bogus");
    assert_unknown_flag(&["wat", &w, "--bogus"], "--bogus");
    assert_eq!(
        wasmrt(&["wat", &w, "-o", out.to_str().unwrap()]).status.code(),
        Some(0),
        "`-o` is recognised"
    );
}
