//! **The exit status is the verdict** — `interop.md` §2.3: success → 0, host-side failure (bad
//! arguments, unreadable file, invalid module) → non-zero, AGREED in both copies of the contract.
//!
//! Measured 2026-09-19 during `coordinate`, running both runtimes: wasmrt broke the agreed row in
//! three places, and wazmrt exited 1 in all three:
//!
//! * `wasmrt <file>` summarized an **invalid** module and exited **0** — so `wasmrt m.wasm && …`
//!   passed every invalid module, and this project's own `.wat` corpus gate was fooled by it twice;
//! * `wasmrt` with **no arguments** printed the version and exited **0**;
//! * `wasmrt wast` exited **0** on a failed assertion, an unparseable script and an unreadable
//!   file — no CI job could gate on it.
//!
//! Each test below keys on the STATUS, because the status is what a script branches on.

use std::process::Command;

fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

fn code(args: &[&str]) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
        .status
        .code()
}

const VALID: &[u8] = br#"(module (func (export "f") (result i32) (i32.const 7)))"#;
const INVALID: &[u8] = br#"(module (func (export "f") (result i32) (i64.const 7)))"#;

#[test]
fn summarize_exits_zero_on_a_valid_module() {
    let p = write_temp("wasmrt_exit_valid.wat", VALID);
    assert_eq!(code(&[p.to_str().unwrap()]), Some(0));
}

/// ⚠️⚠️ The regression this file exists for.
#[test]
fn summarize_exits_nonzero_on_an_invalid_module() {
    let p = write_temp("wasmrt_exit_invalid.wat", INVALID);
    assert_eq!(code(&[p.to_str().unwrap()]), Some(1), "an invalid module must not exit 0");
    // …and for a BINARY, which cannot fail earlier at assembly: the text path above could pass
    // for the wrong reason if the assembler ever started refusing ill-typed text.
    let bin = wasmrt_core::wat::assemble(INVALID).expect("the assembler does not type-check");
    let p = write_temp("wasmrt_exit_invalid.wasm", &bin);
    assert_eq!(code(&[p.to_str().unwrap()]), Some(1));
}

#[test]
fn no_arguments_is_a_usage_error() {
    assert_eq!(code(&[]), Some(1));
    // The explicit spellings are still requests, and still succeed.
    assert_eq!(code(&["--version"]), Some(0));
    assert_eq!(code(&["--help"]), Some(0));
}

#[test]
fn wast_exits_zero_only_when_every_assertion_passed() {
    let pass = write_temp(
        "wasmrt_exit_pass.wast",
        br#"(module (func (export "f") (result i32) (i32.const 1)))
            (assert_return (invoke "f") (i32.const 1))"#,
    );
    let fail = write_temp(
        "wasmrt_exit_fail.wast",
        br#"(module (func (export "f") (result i32) (i32.const 1)))
            (assert_return (invoke "f") (i32.const 2))"#,
    );
    let unparseable = write_temp("wasmrt_exit_unparseable.wast", b"(module (func");
    let missing = std::env::temp_dir().join("wasmrt_exit_no_such_file.wast");
    let _ = std::fs::remove_file(&missing);

    assert_eq!(code(&["wast", pass.to_str().unwrap()]), Some(0));
    assert_eq!(code(&["wast", fail.to_str().unwrap()]), Some(1), "a failed assertion");
    assert_eq!(code(&["wast", unparseable.to_str().unwrap()]), Some(1), "an unparseable script");
    assert_eq!(code(&["wast", missing.to_str().unwrap()]), Some(1), "an unreadable file");
    // One bad file fails the whole run, even beside a good one.
    assert_eq!(
        code(&["wast", pass.to_str().unwrap(), fail.to_str().unwrap()]),
        Some(1)
    );
}
