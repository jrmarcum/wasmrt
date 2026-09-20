//! `--features` must NARROW what every path accepts — not just the WASI one.
//!
//! Found by a validator review on 2026-09-20 while trying to measure the proposal gates from the
//! command line: `wasmrt --features mvp simd.wasm` printed `validation OK`, and
//! `wasmrt run --features mvp simd.wasm f` *executed* the module and printed its answer. The flag
//! was parsed, stored on `HostFlags`, and then read by exactly one of the three paths that judge a
//! module — `wasmrt wasi`. The other two called `validate()`, which is `Features::all()`.
//!
//! 🎓 §4.10, for the second time in two days: **parsing a flag proves it was ACCEPTED, not
//! APPLIED** — so test the SPELLINGS, one per path, and assert on what the command DID.
//! An embedder restricting a guest to the MVP got no restriction at all on two of three paths,
//! and nothing said so.

use std::process::{Command, Output};

fn fixture(name: &str, bytes: &[u8]) -> String {
    let d = std::env::temp_dir().join("wasmrt_features_applied");
    std::fs::create_dir_all(&d).expect("scratch dir");
    let p = d.join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p.to_str().unwrap().to_string()
}

fn wasmrt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmrt"))
        .args(args)
        .output()
        .expect("spawn wasmrt")
}

/// A SIMD module with an i32-returning export, plus `_start` so it can also be run as a WASI
/// command.
const SIMD: &[u8] = br#"(module
  (func (export "f") (result i32)
    v128.const i32x4 7 0 0 0
    i32x4.extract_lane 0)
  (func (export "_start")))"#;

/// ⚠️ The SAME module **without** `_start`. A bare path that exports `_start` is a *run*, not a
/// summary, so the summarize test below was silently exercising the WASI path — the one entry
/// point that already honoured `--features`. It passed against the unfixed code, and the
/// mutation run is what said so. **A fixture that reaches the wrong path reports the right
/// answer for the wrong reason.**
const SIMD_NO_START: &[u8] = br#"(module
  (func (export "f") (result i32)
    v128.const i32x4 7 0 0 0
    i32x4.extract_lane 0))"#;

fn simd_module() -> String {
    fixture("simd.wat", SIMD)
}

fn simd_module_no_start() -> String {
    fixture("simd-no-start.wat", SIMD_NO_START)
}

#[test]
fn summarize_judges_the_module_under_the_configured_features() {
    let m = simd_module_no_start();
    // The control: with everything enabled it is a valid module, so a refusal below is about the
    // feature set and not about the fixture.
    let ok = wasmrt(&[&m]);
    assert!(ok.status.success(), "the module must validate with every proposal enabled");

    let out = wasmrt(&["--features", "mvp", &m]);
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`--features mvp` must make a SIMD module invalid; got:\n{text}"
    );
    assert!(
        !text.contains("validation OK"),
        "a module that uses a disabled proposal must never be reported valid:\n{text}"
    );
    assert!(text.contains("simd"), "the report must name the proposal:\n{text}");
}

#[test]
fn calling_an_export_judges_the_module_under_the_configured_features() {
    let m = simd_module();
    // Both spellings of the same run: the subcommand and the bare path.
    for args in [
        vec!["run", "--features", "mvp", m.as_str(), "f"],
        vec!["--features", "mvp", m.as_str(), "f"],
    ] {
        let out = wasmrt(&args);
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`{args:?}` executed a module that uses a disabled proposal; got:\n{text}"
        );
        // The measured symptom: it ran and printed the answer.
        assert!(!text.contains('7'), "the module must not have executed:\n{text}");
    }
    // …and with the proposal enabled the very same command answers, so the assertions above are
    // about the feature set rather than about a broken command line.
    let out = wasmrt(&["run", "--features", "all", &m, "f"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
}

#[test]
fn the_wasi_path_still_applies_them() {
    let m = simd_module();
    for args in [
        vec!["wasi", "--features", "mvp", m.as_str()],
        vec!["--features", "mvp", m.as_str()],
    ] {
        let out = wasmrt(&args);
        assert!(
            !out.status.success(),
            "`{args:?}` must refuse a SIMD module under mvp"
        );
    }
}
