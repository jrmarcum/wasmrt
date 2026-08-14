//! Runs every `.wast` regression script in the repo's `tests/` directory under `cargo test`.
//!
//! ⚠️ **This exists because a gate with no trigger is a preference, not a gate.** The two
//! cross-module type-identity reproducers were added as `.wast` files and were green — but nothing
//! ran them except a human typing `wasmrt wast tests/`. A regression file that no automated run
//! touches records a bug; it does not defend against it.
//!
//! Anything dropped into `tests/*.wast` from now on is covered automatically, which is the point: the
//! next person to add a reproducer should not have to remember to wire it up.
//!
//! Note the *skipped* assertion. A construct this build cannot put to the test is silently not a
//! pass, so a regression file that quietly stopped being adjudicated — because a construct it
//! depends on regressed into "unsupported" — would otherwise read as success.

use std::path::{Path, PathBuf};

/// The repo-root `tests/` directory, from this crate's manifest dir (`crates/wasmrt-core`).
fn script_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
}

fn scripts() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(script_dir())
        .expect("repo tests/ directory must exist")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wast")))
        .collect();
    v.sort();
    v
}

#[test]
fn every_regression_script_passes_completely() {
    let files = scripts();
    // A zero-file run would pass vacuously, which is the failure mode this whole file is about.
    assert!(
        !files.is_empty(),
        "no .wast regression scripts found in {} — the gate would pass vacuously",
        script_dir().display()
    );

    let mut report = String::new();
    for path in &files {
        let src = std::fs::read(path).expect("read script");
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match wasmrt_core::wast::run_script(&src) {
            Ok(s) => {
                if s.failed != 0 || s.skipped != 0 || s.passed == 0 {
                    report.push_str(&format!(
                        "\n  {name}: {} passed, {} failed, {} skipped{}",
                        s.passed,
                        s.failed,
                        s.skipped,
                        if s.failures.is_empty() {
                            String::new()
                        } else {
                            format!("\n      {}", s.failures.join("\n      "))
                        }
                    ));
                }
            }
            Err(e) => report.push_str(&format!("\n  {name}: script error: {e:?}")),
        }
    }
    assert!(report.is_empty(), "regression scripts not fully green:{report}");
}
