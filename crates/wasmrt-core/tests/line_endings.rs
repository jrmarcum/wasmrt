//! Every text file in the repository is LF, and `cargo test` is what says so.
//!
//! 🔒 **The rule is git's own.** `gitattributes(5)`: *"When a matching file is added to the index,
//! the file's line endings are normalized to LF in the index."* LF is the canonical, in-repository
//! form; CRLF is only ever a working-tree rendering that a checkout may produce. This project
//! standardises on git's form on both sides — `.gitattributes` carries `* text=auto eol=lf`, so the
//! working tree matches what the repository already stores, on every machine and with no
//! per-developer git config.
//!
//! ⚠️ **Why it needs a TEST and not just the attribute.** An attribute is enforced by git at
//! checkout and at commit; it cannot stop an editor, a generator or a careless script from writing
//! CRLF into a file that then sits in the working tree looking perfectly normal. That gap is where
//! the defect actually lived: six mutation tests in one session silently applied nothing, because a
//! pattern containing `\n` cannot match a CRLF file — and a mutation that never applied is
//! indistinguishable from a gate that caught nothing (`cmem/best-practices.md` §8.1b). A gate with
//! no trigger is a preference, so this one runs with every `cargo test`, exactly like
//! `regression_wast.rs`.
//!
//! `deno run -A scripts/eol-gate.ts --fix` rewrites offenders; this test only reports them.
//!
//! ⚠️ **If a fixture ever needs a real CR** — a lexer test for the §6.2 source character set, say —
//! build it in Rust with `\r` or from bytes, rather than embedding a raw CR in a checked-in file.
//! A CR that is *content* is indistinguishable here from a CR that is an accident, which is the
//! whole reason this file can be strict.

use std::path::{Path, PathBuf};

/// The repository root, from this crate's manifest dir (`crates/wasmrt-core`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root")
}

/// Directories that are not source: git's own storage and build output.
fn skip_dir(name: &str) -> bool {
    matches!(name, ".git" | "target" | "node_modules")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !skip_dir(&name) {
                collect(&p, out);
            }
        } else {
            out.push(p);
        }
    }
}

#[test]
fn every_text_file_in_the_repository_uses_lf() {
    let root = repo_root();
    let mut files = Vec::new();
    collect(&root, &mut files);
    // A zero-file walk would pass vacuously — the failure mode `regression_wast.rs` exists to name.
    assert!(
        files.len() > 50,
        "the walk found only {} files under {} — it is not looking at the repository",
        files.len(),
        root.display()
    );

    let mut checked = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for p in &files {
        let Ok(bytes) = std::fs::read(p) else { continue };
        // A NUL means binary, and git does not convert binary files either (`text=auto`).
        if bytes.contains(&0) {
            continue;
        }
        checked += 1;
        if let Some(i) = bytes.iter().position(|&b| b == b'\r') {
            let line = 1 + bytes[..i].iter().filter(|&&b| b == b'\n').count();
            offenders.push(format!(
                "{} (first CR at line {line})",
                p.strip_prefix(&root).unwrap_or(p).display()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "{} of {checked} text files contain a CR; line endings are LF in this repository \
         (`.gitattributes`: `* text=auto eol=lf`). Run `deno run -A scripts/eol-gate.ts --fix`:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}
