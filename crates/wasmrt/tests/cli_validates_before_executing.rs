//! **Every CLI path that executes must validate first.**
//!
//! This is an integration test rather than a unit test because the defect it guards lived in the
//! CLI's argument handling, not in the library: `wasmrt run` decoded and executed without ever
//! calling `validate`, while `wasmrt wasi` next door refused the same bytes. No core-level test
//! could have caught that — the hole was the *absence* of a call at one entry point.
//!
//! The same defect existed in the reference oracle (`wazmrt`) on two of its paths plus its C ABI,
//! and was fixed there in the same session. See `cmem/known-issues.md`.

use std::process::Command;

/// `(func (result i32) i64.const 1)` — a body whose type does not match its declared result.
/// Ill-typed by §3.3.5. Assembled rather than hand-encoded: hand-encoding it got the section sizes
/// wrong and the CLI reported a *decode* failure, which would have made this test pass for the wrong
/// reason — it must be REJECTED AS INVALID, not as malformed.
fn ill_typed_module() -> Vec<u8> {
    wasmrt_core::wat::assemble(br#"(module (func (export "f") (result i32) i64.const 1))"#)
        .expect("the fixture must ASSEMBLE — it is ill-typed, not malformed")
}

fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

/// ⚠️ The regression this exists for: `wasmrt run` used to print `1` and exit 0 here.
#[test]
fn run_refuses_an_invalid_module() {
    let p = write_temp("wasmrt_cli_invalid_run.wasm", &ill_typed_module());
    let out = Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(["run", p.to_str().unwrap(), "f"])
        .output()
        .expect("spawn wasmrt");

    assert!(
        !out.status.success(),
        "`wasmrt run` must refuse an invalid module, but it exited {:?} with stdout {:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("invalid module"),
        "expected an invalidity diagnostic, got: {err}"
    );
    // And it must say WHERE, which is the other half of what this cost to diagnose.
    assert!(
        err.contains("in function 0"),
        "the diagnostic must name the offending function, got: {err}"
    );
}

/// The sibling path, which already validated — pinned so the two cannot drift apart again. An
/// asymmetry between two entry points of one binary is what made the original bug survive.
#[test]
fn wasi_refuses_an_invalid_module() {
    let p = write_temp("wasmrt_cli_invalid_wasi.wasm", &ill_typed_module());
    let out = Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(["wasi", p.to_str().unwrap()])
        .output()
        .expect("spawn wasmrt");
    assert!(!out.status.success(), "`wasmrt wasi` must refuse an invalid module");
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid module"));
}

/// The summarize path reports invalidity as a *verdict* rather than refusing to proceed — it never
/// executes, so it is allowed to describe the module and then say it is invalid.
#[test]
fn summarize_reports_invalidity_as_a_verdict() {
    let p = write_temp("wasmrt_cli_invalid_sum.wasm", &ill_typed_module());
    let out = Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .arg(p.to_str().unwrap())
        .output()
        .expect("spawn wasmrt");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("validation FAILED"),
        "summarize must report the verdict, got: {stdout}"
    );
}

/// The other direction: a VALID module must still run. A guard that refuses everything would pass
/// every assertion above (§4.1 — a gate that cannot fail is decoration).
#[test]
fn run_still_executes_a_valid_module() {
    let m = wasmrt_core::wat::assemble(br#"(module (func (export "f") (result i32) i32.const 7))"#)
        .expect("assemble");
    let p = write_temp("wasmrt_cli_valid_run.wasm", &m);
    let out = Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(["run", p.to_str().unwrap(), "f"])
        .output()
        .expect("spawn wasmrt");
    assert!(out.status.success(), "a valid module must still run");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
}
