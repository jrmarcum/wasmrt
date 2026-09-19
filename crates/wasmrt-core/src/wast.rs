//! `wast` — the WAST **script runner**: the spec testsuite's command language.
//!
//! Handles `(module …)` definitions plus the assertion commands — `assert_return`,
//! `assert_trap`, `assert_exhaustion`, `assert_invalid`, `assert_malformed`,
//! `assert_unlinkable` — and the `invoke` / `get` / `register` actions. Ported from wazmrt
//! `src/wast.zig` (T6).
//!
//! Operates on script **text**; file I/O is the CLI's job, so this stays `std`-free.
//!
//! # The honesty rule
//!
//! **Never count "we couldn't build it" as a pass.** A harness that treats its own gaps as
//! success reports the shape of its gaps as conformance. So:
//!
//! - `assert_invalid` passes only on a **validation** rejection, `assert_malformed` only on
//!   a **decode/parse** rejection. If the assembler simply cannot express the construct
//!   ([`crate::wat::Error::Unsupported`] / `UnknownInstr`), that is a **skip** — the module
//!   was never really put to the test.
//! - `assert_trap` and `assert_exhaustion` accept only a genuine runtime [`Trap`], never a
//!   setup or assembly failure.
//!
//! Skips are counted and reported separately from passes for exactly this reason.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::interp::{externalize, host_ref, Imports, InstanceId, Store, Trap, Value};
use crate::linker::Linker;
use crate::module::Module;
use crate::sexpr::{self, Sexpr};
use crate::wat;

/// Outcome of running a script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    /// Commands this runner could not put to the test (an unsupported construct, a module
    /// that never built, a command kind not handled). **Never** folded into `passed`.
    pub skipped: usize,
    /// Descriptions of the failures, for debugging. Capped so a badly-broken file cannot
    /// produce unbounded output.
    pub failures: Vec<String>,
    /// Why each skip happened. ⚠️⚠️ A skip used to be a bare COUNT, so "1,024 skipped" could
    /// be read but never attributed: scoping the remaining skip work meant guessing from the
    /// file name. A number the report cannot explain is not a measurement.
    pub skips: Vec<String>,
}

impl Summary {
    /// Total commands that were actually adjudicated.
    #[must_use]
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} passed, {} failed, {} skipped",
            self.passed, self.failed, self.skipped
        )
    }
}

/// A script-level failure (the source could not be parsed at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Parse(sexpr::ParseError),
}

impl From<sexpr::ParseError> for Error {
    fn from(e: sexpr::ParseError) -> Self {
        Error::Parse(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "s-expression parse error: {e}"),
        }
    }
}

impl core::error::Error for Error {}

/// Cap on recorded failure descriptions.
const MAX_RECORDED_FAILURES: usize = 25;
/// Skips are far denser than failures — one unbuildable module skips every assertion behind
/// it, and the worst file in the corpus carries 108. The cap sits well clear of that so the
/// reason list is COMPLETE in practice; a cap that silently truncated would reintroduce the
/// exact blindness `Summary::skips` was added to remove.
const MAX_RECORDED_SKIPS: usize = 512;

/// Parse and run a whole `.wast` source.
///
/// # Errors
/// Returns [`Error::Parse`] if the source is not well-formed s-expressions. Command-level
/// problems are counted in the [`Summary`] rather than returned.
pub fn run_script(src: &[u8]) -> Result<Summary, Error> {
    let (forms, top_annots) = sexpr::parse_all_annotated(src)?;
    let mut r = Runner::default();
    let mut i = 0;
    while i < forms.len() {
        // §7's **inline module** abbreviation: a run of bare MODULE FIELDS at the top level of a
        // script is one module, with the `(module …)` wrapper left off. `inline-module.wast` is a
        // single line — `(func) (memory 0) (func (export "f"))` — and is a whole module.
        //
        // ⚠️ Handled here, not in `command`: the abbreviation is about a RUN of forms, and a
        // dispatcher that sees one command at a time can only report each as an unhandled one,
        // which is what it did. Three assertions, and the reason they read as three unrelated
        // "unhandled command `func`" skips rather than one missing feature.
        if is_module_field(&forms[i]) {
            let start = i;
            while i < forms.len() && is_module_field(&forms[i]) {
                i += 1;
            }
            let mut inline = alloc::vec![Sexpr::Atom(String::from("module"))];
            inline.extend_from_slice(&forms[start..i]);
            // Annotations between the fields belong to the module they sit in, shifted one
            // place for the `module` keyword prepended above.
            let annots = top_annots
                .iter()
                .filter(|a| (start..=i).contains(&a.before))
                .map(|a| sexpr::Annot { before: a.before - start + 1, ..a.clone() })
                .collect();
            r.command(&Sexpr::List(inline, annots));
            continue;
        }
        r.command(&forms[i]);
        i += 1;
    }
    Ok(r.summary)
}

/// Is this top-level form a **module field** rather than a script command (§6.6.13)?
///
/// The closed list of what may appear inside `(module …)`. Deliberately not "anything that is not
/// a known command": an unknown command must stay an unknown command, so the report keeps saying
/// so instead of quietly assembling it into a module and failing somewhere else.
fn is_module_field(form: &Sexpr) -> bool {
    matches!(
        form.keyword(),
        Some(
            "type"
                | "rec"
                | "import"
                | "func"
                | "table"
                | "memory"
                | "global"
                | "tag"
                | "export"
                | "start"
                | "elem"
                | "data"
        )
    )
}

#[derive(Default)]
struct Runner {
    /// One store for the whole script, so `(register …)` can publish a module and a later
    /// one can import from it — the instances genuinely share resources.
    store: Store,
    /// The most recently built module, which un-named actions target.
    current: Option<InstanceId>,
    /// Whether the most recent `(module …)` **failed to build**.
    ///
    /// Distinct from `current == None`, which also means "the last module was named".
    /// Without the distinction, the fall-back in [`Runner::target`] sends a failed
    /// module's assertions to an unrelated earlier instance, which then reports them as
    /// value mismatches — a bug hunt aimed at a defect that does not exist.
    last_build_failed: bool,
    /// Modules by their textual `$name`, for `(invoke $M …)`.
    named: Vec<(String, InstanceId)>,
    /// Modules published by `(register "name")`, which later modules may import from.
    registered: Vec<(String, InstanceId)>,
    /// The instance that owns `spectest`'s exported memory, built on first use.
    ///
    /// A memory's identity in this engine *is* a store slot, so `spectest.memory` cannot be
    /// conjured from a factory the way its `print*` functions are — something must own it. This
    /// is that owner: a one-field module instantiated into the same store, so a guest importing
    /// `(memory 1 2)` genuinely shares its bytes.
    spectest_mem: Option<InstanceId>,
    /// `(module definition $M …)` — assembled bytes held for a later `(module instance …)`,
    /// deliberately NOT instantiated.
    definitions: Vec<(String, Vec<u8>)>,
    summary: Summary,
}

/// The functions the spec suite's `spectest` host module exports (§ the suite's own README).
///
/// ⚠️ **A closed set, not a namespace.** These were installed with `define_namespace`, which makes
/// EVERY name under `spectest` resolvable — so `(import "spectest" "unknown" (func))` linked, and
/// `imports.wast` asserts it must not. A catch-all is the right tool for an embedder that wants
/// unsatisfied imports to trap when called; it is the wrong one for a module whose exports are
/// written down.
const SPECTEST_FUNCS: &[&str] = &[
    "print",
    "print_i32",
    "print_i64",
    "print_f32",
    "print_f64",
    "print_i32_f32",
    "print_f64_f64",
];

/// Why an action could not run.
enum ActionErr {
    /// The module never built, or the named target is unknown — nothing was tested.
    NoTarget,
    /// A genuine runtime trap.
    Trap(Trap),
    /// The action form itself was malformed.
    Bad(String),
}

impl Runner {
    fn fail(&mut self, msg: String) {
        self.summary.failed += 1;
        if self.summary.failures.len() < MAX_RECORDED_FAILURES {
            self.summary.failures.push(msg);
        }
    }

    /// Record a skip WITH its reason. Every `skipped += 1` goes through here: a skip site
    /// that only bumps the counter is a hole in the report.
    fn skip(&mut self, msg: String) {
        self.summary.skipped += 1;
        if self.summary.skips.len() < MAX_RECORDED_SKIPS {
            self.summary.skips.push(msg);
        }
    }

    fn command(&mut self, cmd: &Sexpr) {
        let Some(kw) = cmd.keyword() else {
            self.skip(String::from("command: no leading keyword"));
            return;
        };
        let list = cmd.as_list().unwrap_or(&[]);
        match kw {
            "module" => self.define_module(cmd),
            "assert_return" => self.assert_return(list),
            "assert_trap" => self.assert_trap(list),
            "assert_exhaustion" => self.assert_exhaustion(list),
            "assert_exception" => self.assert_exception(list),
            "assert_invalid" => self.assert_rejected(list, Rejection::Invalid),
            "assert_malformed" => self.assert_rejected(list, Rejection::Malformed),
            "assert_unlinkable" => self.assert_unlinkable(list),
            "assert_malformed_custom" => self.assert_malformed_custom(list),
            "assert_invalid_custom" => self.assert_invalid_custom(list),
            "invoke" | "get" => match self.run_action(cmd) {
                Ok(_) => self.summary.passed += 1,
                Err(ActionErr::NoTarget) => {
                    self.skip(format!("{kw}: no target instance (an earlier module did not build)"));
                }
                Err(ActionErr::Trap(t)) => self.fail(format!("action trapped: {t}")),
                Err(ActionErr::Bad(m)) => self.fail(m),
            },
            // `(register "name" $id?)` publishes a module's exports under `name`, so later
            // modules can import from it: the $id-named module if given, else the current one.
            "register" => {
                let explicit = list
                    .get(2)
                    .and_then(Sexpr::as_atom)
                    .filter(|a| a.starts_with('$'))
                    .and_then(|a| self.named.iter().find(|(n, _)| n == a).map(|(_, i)| *i));
                // ⚠️ With no `$id`, register the MOST RECENT module — which may be a named one.
                // This used to be `.or(self.current)`, and `current` is deliberately `None`
                // after a named module, so `(module $M …) (register "M")` registered **nothing**
                // and every later import of `"M"` failed to link. `load1.wast` is exactly that
                // shape: one failed build plus every assertion behind it.
                //
                // 🎓 `target()` next door has carried the same fallback all along — *a
                // classification rule that one call site does not consult is a rule with an
                // exception nobody wrote down*. Delegating keeps it one rule, including the
                // `last_build_failed` guard, which register needs for the same reason actions do.
                let target = explicit.or_else(|| self.target(None));
                match (list.get(1).and_then(Sexpr::as_str), target) {
                    (Some(name), Some(id)) => {
                        let name = String::from_utf8_lossy(name).into_owned();
                        self.registered.push((name, id));
                        self.summary.passed += 1;
                    }
                    _ => self.skip(String::from(
                        "register: no target instance (an earlier module did not build)",
                    )),
                }
            }
            _ => self.skip(format!("unhandled command `{kw}`")),
        }
    }

    /// Assemble a `(module …)` form to bytes. Handles the `binary` and `quote` variants.
    fn module_binary(node: &Sexpr) -> Result<Vec<u8>, wat::Error> {
        let form = node.as_list().unwrap_or(&[]);
        let quote_at = form.iter().position(|s| s.as_atom() == Some("quote"));
        if let Some(q) = quote_at {
            let mut body = Vec::new();
            for s in &form[q + 1..] {
                body.extend_from_slice(s.as_str().unwrap_or(&[]));
                body.push(b'\n');
            }
            return wat::assemble(&Self::quoted_module_source(body));
        }
        wat::assemble_form(node)
    }

    /// The `.wat` source a `(module quote "…")` denotes. The text is EITHER a module's fields
    /// or a whole `(module …)` form — the spec grammar is `module ::= module_ | inline_module`
    /// — and only the first needs wrapping.
    ///
    /// ⚠️ This used to wrap unconditionally, so every whole-module quote became
    /// `(module (module …))` and was refused with `BadModuleField` whatever it contained. Inside
    /// `assert_malformed` that refusal SCORED AS A PASS — 112 assertions (`align.wast` 46,
    /// `align64.wast` 46, `start`, `try_table`, `legacy/`, `custom/`) were being passed by a
    /// wrapper, never reaching the rule they test. Outside one, a valid module failed to build.
    fn quoted_module_source(body: Vec<u8>) -> Vec<u8> {
        let whole = matches!(
            crate::sexpr::parse_all(&body).as_deref(),
            Ok([only]) if only.keyword() == Some("module")
        );
        if whole {
            return body;
        }
        let mut src = b"(module\n".to_vec();
        src.extend_from_slice(&body);
        src.extend_from_slice(b")\n");
        src
    }

    /// Build the [`Linker`] this script links against: the standard `spectest` host module
    /// plus every module published by `(register …)`.
    ///
    /// Rebuilt per module rather than kept on the `Runner`, because `(register …)` can add
    /// a namespace between two builds and the linker must see it.
    fn linker(&self) -> Linker {
        let mut l = Linker::new();
        // The `print*` functions exist only to be callable — the suite asserts nothing about what
        // they emit — so one empty body serves all of them; the NAMES are what matter.
        for name in SPECTEST_FUNCS {
            l.define_func("spectest", name, |_caller, _args, _results| Ok(()));
        }
        // The suite checks these values, so they must be exact. Named explicitly rather
        // than produced by a factory: it is a closed, known set.
        for (name, v) in [
            ("global_i32", crate::interp::i32_value(666)),
            ("global_i64", crate::interp::i64_value(666)),
            ("global_f32", crate::interp::f32_value(666.6)),
            ("global_f64", crate::interp::f64_value(666.6)),
        ] {
            l.define_global("spectest", name, v);
        }
        if let Some(id) = self.spectest_mem {
            l.define_memory("spectest", "memory", id, 0);
            // The threads proposal's `spectest` adds a SHARED memory beside the plain one, and
            // `proposals/threads/imports.wast` needs both at once: it imports `shared_memory` as
            // shared (must link), then asserts that importing it UNSHARED is unlinkable and that
            // importing plain `memory` as shared is unlinkable. ⚠️ The second of those three was
            // already scored a PASS while this export was missing — an unresolvable import is a
            // link failure, and `assert_unlinkable` asks only that linking fail. It was right for
            // the wrong reason, and only defining the export makes it test the `shared` flag it
            // was written to test.
            l.define_memory("spectest", "shared_memory", id, 1);
            l.define_table("spectest", "table", id, 0);
            // `table64` is the 64-bit twin the memory64 proposal's spectest adds; `table64.wast`
            // imports it and, without it, the whole file failed at link.
            l.define_table("spectest", "table64", id, 1);
        }
        for (name, id) in &self.registered {
            l.define_instance(name, *id);
        }
        l
    }

    /// Ensure `spectest`'s memory exists, so [`Runner::linker`] can name it.
    ///
    /// `(memory 1 2)` is the type the suite declares for it — the exact limits matter, because
    /// `imports.wast` asserts that importing it with a *wider* type is unlinkable. Built lazily
    /// so a script that never mentions it pays nothing, and once, so every importer in a script
    /// shares one memory (which is what `spectest` means).
    fn ensure_spectest_memory(&mut self) {
        if self.spectest_mem.is_some() {
            return;
        }
        // Assembled from source rather than hand-built bytes: it goes through the same
        // assembler the suite's own modules do, so it cannot drift from what that accepts.
        // `(memory 1 2)` and `(table 10 20 funcref)` are the types the suite declares for spectest's
        // exports, and the exact limits matter: `imports.wast` asserts that importing either with a
        // *wider* type is unlinkable. Both live in one owner module because a memory's and a table's
        // identity in this engine is a store slot, so something must own them.
        let Ok(bytes) = crate::wat::assemble(
            b"(module (memory (export \"memory\") 1 2) (memory (export \"shared_memory\") 1 2 shared)               (table (export \"table\") 10 20 funcref) (table (export \"table64\") i64 10 20 funcref))",
        ) else {
            return;
        };
        let Ok(md) = crate::module::decode(&bytes) else {
            return;
        };
        if let Ok(id) = self.store.instantiate(md, Imports::new()) {
            self.spectest_mem = Some(id);
        }
    }

    /// Resolve a module's declared imports against `spectest` and the registered modules.
    ///
    /// Delegates to [`Linker`], which walks them in **declaration order** so each backing
    /// binds to its own slot. Sharing that walk with the C ABI and WASI is the point:
    /// binding two same-kind imports in the wrong order links fine and misroutes every
    /// call, so it must be written once.
    fn resolve_imports(&mut self, md: &Module) -> Result<Imports, BuildErr> {
        // An imported table still has no correct backing (T9a#4), and `LinkError` does not
        // distinguish "unresolved" finely enough for the runner's skip accounting — so every
        // link failure collapses to `Unresolved`, exactly as before.
        self.ensure_spectest_memory();
        self.linker()
            .resolve(&self.store, md)
            .map_err(|e| match e {
                crate::linker::LinkError::UnsupportedImportKind(k) => BuildErr::UnsupportedLink(k),
                other => BuildErr::Unlinkable(other),
            })
    }

    fn build(&mut self, node: &Sexpr) -> Result<InstanceId, BuildErr> {
        let bytes = Self::module_binary(node).map_err(BuildErr::Assemble)?;
        self.instantiate_bytes(&bytes)
    }

    /// assemble → decode → **validate, and stop**. The pipeline `assert_invalid` and
    /// `assert_malformed` actually ask about.
    ///
    /// ⚠️⚠️ **Both assertions used to run the FULL [`Self::build`], through link and
    /// instantiation, and that made the runner misreport.** `assert_invalid` ends at
    /// validation by definition — a module that validates has already answered the
    /// assertion, and what happens to its imports afterwards is not part of the question.
    /// Running on regardless meant a module whose imports nothing provides came back as
    /// `Unlinkable`, which [`Rejection::accepts`] rightly refuses, and the failure printed
    /// *"rejected at the wrong stage (link: unknown import)"* — blaming the engine for
    /// reaching a stage the runner should never have taken it to.
    ///
    /// Found by the Track M/A triage (2026-08-20): 4 of `proposals/threads/imports.wast`'s
    /// 13 failures were this, wearing a conformance-failure label. 🎓 The count does not
    /// move — those 4 still fail, because the modules are era-pinned assertions a modern
    /// engine must reject — but they now fail with the TRUE reason (*"module was
    /// accepted"*), which is what makes them attributable to the snapshot rather than to us.
    /// **The misreport is corpus-wide and would resurface the moment another file pairs an
    /// unresolvable import with a validity assertion**, which is why this is fixed on its own
    /// merits rather than waiting for the snapshot refresh to hide it.
    fn build_to_validation(&mut self, node: &Sexpr) -> Result<(), BuildErr> {
        let bytes = Self::module_binary(node).map_err(BuildErr::Assemble)?;
        let md = crate::module::decode(&bytes).map_err(BuildErr::Decode)?;
        crate::validate::validate(&md).map_err(BuildErr::Validate)
    }

    /// decode → validate → link → instantiate, from module bytes.
    ///
    /// Split out of [`Self::build`] so `(module instance $I $M)` can instantiate a stored
    /// definition **again** — instantiation is generative, and a second instance must get its
    /// own globals, tables and memories rather than a handle to the first.
    fn instantiate_bytes(&mut self, bytes: &[u8]) -> Result<InstanceId, BuildErr> {
        let md = crate::module::decode(bytes).map_err(BuildErr::Decode)?;
        crate::validate::validate(&md).map_err(BuildErr::Validate)?;
        let imports = self.resolve_imports(&md)?;
        self.store
            .instantiate(md, imports)
            .map_err(BuildErr::Instantiate)
    }

    /// `(module definition $M …)` and `(module instance $I $M)` — §: instantiation is
    /// **generative**, so the suite needs to define a module once and instantiate it twice,
    /// asserting the two instances have separate state.
    ///
    /// Returns `true` when the form was one of these, so the ordinary path is skipped.
    /// ⚠️ A `definition` is **assembled but NOT instantiated** — that is the whole distinction,
    /// and instantiating it here would make `instance.wast`'s generativity assertions pass for
    /// the wrong reason by giving every `instance` the definition's own state.
    fn try_module_definition_or_instance(&mut self, node: &Sexpr) -> bool {
        let list = node.as_list().unwrap_or(&[]);
        // The optional `$name` may precede the keyword: `(module $M definition …)` does not
        // occur, but `(module definition $M …)` does, so scan both positions.
        let kw_at = list
            .iter()
            .take(3)
            .position(|s| matches!(s.as_atom(), Some("definition" | "instance")));
        let Some(k) = kw_at.filter(|&k| k > 0) else {
            return false;
        };
        let kw = list[k].as_atom().unwrap_or("");
        let name = list
            .get(k + 1)
            .and_then(Sexpr::as_atom)
            .filter(|a| a.starts_with('$'))
            .map(str::to_string);
        if kw == "definition" {
            // Assemble the remaining fields as an ordinary module, but only STORE the bytes.
            let skip = k + 1 + usize::from(name.is_some());
            let mut form: Vec<Sexpr> = alloc::vec![Sexpr::Atom(String::from("module"))];
            form.extend(list[skip..].iter().cloned());
            // The fields keep their annotations, shifted to where the fields now stand.
            let annots = node
                .annotations()
                .iter()
                .filter(|a| a.before >= skip)
                .map(|a| sexpr::Annot { before: a.before - skip + 1, ..a.clone() })
                .collect();
            match crate::wat::assemble_form(&Sexpr::List(form, annots)) {
                Ok(bytes) => {
                    if let Some(n) = name {
                        self.definitions.push((n, bytes));
                    }
                }
                Err(e) if BuildErr::Assemble(e.clone()).is_unsupported() => {
                    self.skip(format!("module definition: unsupported ({e})"));
                }
                Err(e) => self.fail(format!("module definition failed to assemble: {e}")),
            }
            return true;
        }
        // `(module instance $I $M)` — instantiate a previously-defined module afresh.
        let of = list
            .get(k + 1 + usize::from(name.is_some()))
            .and_then(Sexpr::as_atom)
            .unwrap_or("");
        let Some((_, bytes)) = self.definitions.iter().find(|(n, _)| n == of) else {
            self.skip(format!("module instance: no definition named `{of}`"));
            return true;
        };
        let bytes = bytes.clone();
        self.last_build_failed = false;
        match self.instantiate_bytes(&bytes) {
            Ok(inst) => {
                if let Some(n) = name {
                    self.named.push((n, inst));
                    self.current = None;
                } else {
                    self.current = Some(inst);
                }
            }
            Err(e) => {
                if e.is_unsupported() {
                    self.skip(format!("module instance: unsupported ({e})"));
                } else {
                    self.fail(format!("module instance failed to build: {e}"));
                }
                self.current = None;
                self.last_build_failed = true;
            }
        }
        true
    }

    fn define_module(&mut self, node: &Sexpr) {
        let list = node.as_list().unwrap_or(&[]);
        if self.try_module_definition_or_instance(node) {
            return;
        }
        self.last_build_failed = false;
        match self.build(node) {
            Ok(inst) => {
                // Track by textual `$name` for later `$M` references.
                if let Some(name) = list.get(1).and_then(Sexpr::as_atom) {
                    if name.starts_with('$') {
                        self.named.push((name.to_string(), inst));
                        // The most recent module is also `current`; clone-free by
                        // re-building is wasteful, so `current` tracks "the last one" via
                        // the named list when a name is present.
                        self.current = None;
                        return;
                    }
                }
                self.current = Some(inst);
            }
            Err(e) => {
                // A module that does not build is a real failure UNLESS the assembler
                // simply cannot express it yet — that is a gap, not a conformance result.
                if e.is_unsupported() {
                    self.skip(format!("module: unsupported ({e})"));
                } else {
                    self.fail(format!("module failed to build: {e}"));
                }
                self.current = None;
                self.last_build_failed = true;
            }
        }
    }

    /// The instance an action targets: `$name` if given, else the most recent module.
    fn target(&mut self, name: Option<&str>) -> Option<InstanceId> {
        match name {
            Some(n) => self.named.iter().find(|(k, _)| k == n).map(|(_, i)| *i),
            // With no un-named current module, fall back to the most recent named one — a
            // `.wast` file that names every module still runs its bare actions. But NOT
            // after a failed build: those assertions belong to the module that failed, and
            // running them against a different instance reports a wrong value instead of
            // "nothing was tested".
            None if self.last_build_failed => None,
            None => self.current.or_else(|| self.named.last().map(|(_, i)| *i)),
        }
    }

    /// Run `(invoke $M? "name" arg*)` or `(get $M? "name")`.
    fn run_action(&mut self, action: &Sexpr) -> Result<Vec<Value>, ActionErr> {
        let l = action
            .as_list()
            .ok_or_else(|| ActionErr::Bad("action is not a list".to_string()))?;
        let kw = l.first().and_then(Sexpr::as_atom).unwrap_or("");
        let mut j = 1;
        let module_name = l
            .get(j)
            .and_then(Sexpr::as_atom)
            .filter(|a| a.starts_with('$'))
            .map(ToString::to_string);
        if module_name.is_some() {
            j += 1;
        }
        let export = l
            .get(j)
            .and_then(Sexpr::as_str)
            .ok_or_else(|| ActionErr::Bad("action: missing export name".to_string()))?
            .to_vec();
        let export = String::from_utf8_lossy(&export).into_owned();
        j += 1;

        let mut args = Vec::new();
        if kw == "invoke" {
            for a in &l[j..] {
                args.push(parse_const(a).map_err(ActionErr::Bad)?);
            }
        }

        let inst = self
            .target(module_name.as_deref())
            .ok_or(ActionErr::NoTarget)?;
        if kw == "get" {
            // `(get $M? "name")` reads an exported GLOBAL — an action, not a call.
            //
            // ⚠️⚠️ This returned `NoTarget`, so 11 assertions were reported as "an earlier module
            // did not build" in two files where every module built fine. A skip reason that names
            // the wrong cause is worse than a bare counter: the census reads it, believes it, and
            // scopes the work behind a module-build problem that does not exist. `Store` has had
            // `export_global` since T8 — the action was simply never wired to it.
            return self
                .store
                .export_global(inst, &export)
                .map(|v| alloc::vec![v])
                .ok_or(ActionErr::NoTarget);
        }
        self.store
            .invoke(inst, &export, &args)
            .map_err(ActionErr::Trap)
    }

    fn assert_return(&mut self, form: &[Sexpr]) {
        let Some(action) = form.get(1) else {
            self.fail("assert_return: missing action".to_string());
            return;
        };
        let results = match self.run_action(action) {
            Ok(r) => r,
            Err(ActionErr::NoTarget) => {
                self.skip(String::from(
                    "assert_return: no target instance (an earlier module did not build)",
                ));
                return;
            }
            Err(ActionErr::Trap(t)) => {
                self.fail(format!("assert_return {}: unexpected trap {t}", render(action)));
                return;
            }
            Err(ActionErr::Bad(m)) => {
                self.fail(m);
                return;
            }
        };
        let expected = &form[2..];
        // A `v128` is ONE value slot here (wasmrt's 128-bit slot), so the result count and
        // the expectation count compare directly — the oracle needs a slot-vs-form
        // adjustment because it stores a v128 as two `u64`s.
        if results.len() != expected.len() {
            self.fail(format!(
                "assert_return {}: arity {} != expected {}",
                render(action),
                results.len(),
                expected.len()
            ));
            return;
        }
        for (got, exp) in results.iter().zip(expected) {
            match value_matches(*got, exp) {
                Ok(true) => {}
                Ok(false) => {
                    self.fail(format!(
                        "assert_return {}: result mismatch (got 0x{got:x}, expected {})",
                        render(action),
                        render(exp)
                    ));
                    return;
                }
                Err(m) => {
                    self.fail(m);
                    return;
                }
            }
        }
        self.summary.passed += 1;
    }

    fn assert_trap(&mut self, form: &[Sexpr]) {
        let Some(operand) = form.get(1) else {
            self.fail("assert_trap: missing operand".to_string());
            return;
        };
        // `assert_trap (module …)` — instantiation itself must trap (an active data or
        // element segment out of bounds, say). It does not become the current module.
        if operand.keyword() == Some("module") {
            match self.build(operand) {
                Ok(_) => self.fail("assert_trap: module instantiated without trapping".to_string()),
                Err(BuildErr::Instantiate(_)) => self.summary.passed += 1,
                Err(e) if e.is_unsupported() => {
                    self.skip(format!("assert_trap: unsupported ({e})"));
                }
                Err(e) => self.fail(format!("assert_trap: non-trap error {e}")),
            }
            return;
        }
        match self.run_action(operand) {
            Ok(_) => self.fail(format!(
                "assert_trap {}: expected a trap, got a result",
                render(operand)
            )),
            Err(ActionErr::Trap(_)) => self.summary.passed += 1,
            Err(ActionErr::NoTarget) => {
                self.skip(String::from("assert_trap: no target instance"));
            }
            Err(ActionErr::Bad(m)) => self.fail(m),
        }
    }

    fn assert_exhaustion(&mut self, form: &[Sexpr]) {
        let Some(action) = form.get(1) else {
            self.fail("assert_exhaustion: missing action".to_string());
            return;
        };
        match self.run_action(action) {
            Ok(_) => self.fail("assert_exhaustion: expected exhaustion, got a result".to_string()),
            Err(ActionErr::Trap(Trap::CallStackExhausted)) => self.summary.passed += 1,
            Err(ActionErr::Trap(t)) => self.fail(format!("assert_exhaustion: got {t}")),
            Err(ActionErr::NoTarget) => {
                self.skip(String::from("assert_exhaustion: no target instance"));
            }
            Err(ActionErr::Bad(m)) => self.fail(m),
        }
    }

    /// `assert_exception action` — the action must raise an exception that **nothing catches**
    /// (EH).
    ///
    /// ⚠️ Distinguished from a plain trap on purpose. An uncaught exception and a division by zero
    /// both end a call, and scoring either as "it stopped, so it passed" would let a handler bug
    /// hide behind an unrelated trap — `try_table.wast` asserts both against the same exports.
    /// [`Trap::UncaughtException`] is the one outcome this command accepts.
    ///
    /// 41 assertions, all of them skipped as an `unhandled command` until 2026-08-20: the runner
    /// simply had no arm for it, and the skip census is what surfaced that it was a whole command
    /// rather than a scatter of unrelated gaps.
    fn assert_exception(&mut self, form: &[Sexpr]) {
        let Some(action) = form.get(1) else {
            self.fail("assert_exception: missing action".to_string());
            return;
        };
        match self.run_action(action) {
            Ok(_) => self.fail(format!(
                "assert_exception {}: expected an uncaught exception, got a result",
                render(action)
            )),
            Err(ActionErr::Trap(Trap::UncaughtException)) => self.summary.passed += 1,
            Err(ActionErr::Trap(t)) => self.fail(format!(
                "assert_exception {}: expected an uncaught exception, got {t}",
                render(action)
            )),
            Err(ActionErr::NoTarget) => {
                self.skip(String::from("assert_exception: no target instance"));
            }
            Err(ActionErr::Bad(m)) => self.fail(m),
        }
    }

    /// `assert_invalid` / `assert_malformed (module …) "reason"` — the module must be
    /// rejected, and by the **right stage**.
    fn assert_rejected(&mut self, form: &[Sexpr], kind: Rejection) {
        let Some(inner) = form.get(1).filter(|s| s.keyword() == Some("module")) else {
            self.skip(format!("{kind:?}: operand is not a (module …)"));
            return;
        };
        match self.build_to_validation(inner) {
            // Quote the spec's own reason string. Without it every over-acceptance in a file
            // reads identically and triaging means hand-matching failures back to source.
            Ok(_) => self.fail(format!(
                "{kind:?}: module was accepted (should be rejected: {})",
                match form.get(2) {
                    // The reason is a string literal, so it arrives decoded to bytes.
                    Some(Sexpr::Str(b)) => String::from_utf8_lossy(b).into_owned(),
                    Some(Sexpr::Atom(a)) => a.clone(),
                    _ => String::from("<no reason given>"),
                }
            )),
            Err(e) => {
                // Only the matching rejection stage counts. An assembler gap is a SKIP:
                // the module was never really put to the test, and scoring it as a pass
                // would make missing features look like conformance.
                if e.is_unsupported() {
                    self.skip(format!("{kind:?}: unsupported ({e})"));
                } else if kind.accepts(&e) {
                    self.summary.passed += 1;
                } else {
                    self.fail(format!("{kind:?}: rejected at the wrong stage ({e})"));
                }
            }
        }
    }

    /// `assert_unlinkable (module …) "reason"` — the module is well-formed and valid, but must
    /// fail to **link**.
    ///
    /// The stage is the whole assertion, exactly as for `assert_invalid` / `assert_malformed`:
    /// a module we turn away at assembly, decoding or validation did not demonstrate an
    /// unlinkable *link*, so that is scored a failure, not a pass. Anything wasmrt cannot back
    /// at all stays a skip.
    fn assert_unlinkable(&mut self, form: &[Sexpr]) {
        let Some(inner) = form.get(1).filter(|s| s.keyword() == Some("module")) else {
            self.skip(String::from("Unlinkable: operand is not a (module …)"));
            return;
        };
        match self.build(inner) {
            Ok(_) => self.fail(format!(
                "Unlinkable: module linked (should fail to link: {})",
                match form.get(2) {
                    Some(Sexpr::Str(b)) => String::from_utf8_lossy(b).into_owned(),
                    Some(Sexpr::Atom(a)) => a.clone(),
                    _ => String::from("<no reason given>"),
                }
            )),
            Err(e) if e.is_unsupported() => {
                self.skip(format!("Unlinkable: unsupported ({e})"));
            }
            Err(e) if e.is_link_failure() => self.summary.passed += 1,
            Err(e) => self.fail(format!("Unlinkable: rejected before linking ({e})")),
        }
    }

    /// `assert_malformed_custom (module …) "reason"` — a CUSTOM ANNOTATION in the module is
    /// malformed or misplaced.
    ///
    /// 🔒 Only [`wat::Error::Annotation`] satisfies it. Any other rejection means the module was
    /// refused for something else and the annotation rule was never reached — the shape of the
    /// 112 false passes the whole-module-quote wrapper produced on 2026-09-19.
    ///
    /// The module is REFUSED, not merely warned about, because that is what canonical tooling
    /// does: wasm-tools (the `wat` crate wasmtime reads text with) raises a parse error for every
    /// case in the spec suite. wasmtime's own `wast` runner parses this command and declines to
    /// score it (`unimplemented wast directives`).
    fn assert_malformed_custom(&mut self, form: &[Sexpr]) {
        let reason = reason_text(form.get(2));
        let Some(inner) = form.get(1).filter(|s| s.keyword() == Some("module")) else {
            self.skip(String::from("assert_malformed_custom: operand is not a (module …)"));
            return;
        };
        match Self::module_binary(inner) {
            // The RULE must be the one asserted, not merely some annotation rule — the reason
            // is prefix-matched, as the reference interpreter matches it.
            Err(wat::Error::Annotation(why)) if why.starts_with(reason.as_str()) => {
                self.summary.passed += 1;
            }
            Err(wat::Error::Annotation(why)) => self.fail(format!(
                "assert_malformed_custom: refused by the wrong rule (`{why}`; expected: {reason})"
            )),
            Ok(_) => self.fail(format!(
                "assert_malformed_custom: module was accepted (should be rejected: {reason})"
            )),
            Err(e) if BuildErr::Assemble(e.clone()).is_unsupported() => {
                self.skip(format!("assert_malformed_custom: unsupported ({e})"));
            }
            Err(e) => self.fail(format!(
                "assert_malformed_custom: rejected for a non-annotation reason ({e}; expected: {reason})"
            )),
        }
    }

    /// `assert_invalid_custom (module …) "reason"` — the module is VALID, and one of its custom
    /// sections is not.
    ///
    /// Both halves are checked. A custom section's errors must not invalidate a module, so the
    /// module has to build and validate; then [`branch_hint_diagnostics`] — the only custom
    /// section with validity rules wasmrt knows — has to find the problem. A module refused
    /// outright fails the assertion: that would be the wrong answer to "is this module valid?".
    fn assert_invalid_custom(&mut self, form: &[Sexpr]) {
        let reason = reason_text(form.get(2));
        let Some(inner) = form.get(1).filter(|s| s.keyword() == Some("module")) else {
            self.skip(String::from("assert_invalid_custom: operand is not a (module …)"));
            return;
        };
        let built = Self::module_binary(inner)
            .map_err(BuildErr::Assemble)
            .and_then(|bytes| {
                let md = crate::module::decode(&bytes).map_err(BuildErr::Decode)?;
                crate::validate::validate(&md).map_err(BuildErr::Validate)?;
                Ok(branch_hint_diagnostics(&bytes, &md))
            });
        match built {
            Ok(d) if d.iter().any(|m| m.starts_with(reason.as_str())) => self.summary.passed += 1,
            Ok(d) => self.fail(format!(
                "assert_invalid_custom: no matching custom-section problem ({d:?}; expected: {reason})"
            )),
            Err(e) if e.is_unsupported() => {
                self.skip(format!("assert_invalid_custom: unsupported ({e})"));
            }
            Err(e) => self.fail(format!(
                "assert_invalid_custom: the module itself was refused ({e}), but a custom section's \
                 errors must not invalidate it (expected: {reason})"
            )),
        }
    }
}

/// An assertion's reason string, for a failure message.
fn reason_text(s: Option<&Sexpr>) -> String {
    match s {
        Some(Sexpr::Str(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(Sexpr::Atom(a)) => a.clone(),
        _ => String::from("<no reason given>"),
    }
}

/// Problems in a module's `metadata.code.branch_hint` section, as the branch-hinting proposal
/// defines them: a hint must name a DEFINED function, and must stand on the first byte of an
/// `if` or `br_if` in that function's body. Each problem is one message; empty means sound.
///
/// ⚠️ **Reported, never enforced.** A custom section's errors must not invalidate a module, and
/// wasm-tools accepts `(@metadata.code.branch_hint "\01") i32.eq` — so this answers the spec
/// suite's `assert_invalid_custom` without changing whether wasmrt runs anything.
///
/// Kept in the runner rather than the decoder: it is a diagnostic, not an engine rule, and the
/// engine's size is what the contest is judged on.
fn branch_hint_diagnostics(bytes: &[u8], md: &crate::module::Module) -> Vec<String> {
    use crate::types::SectionId;
    use crate::opcode::Op;
    let mut out = Vec::new();
    let uleb = |b: &[u8], p: &mut usize| -> Option<usize> {
        let (mut v, mut shift) = (0usize, 0u32);
        loop {
            let x = *b.get(*p)?;
            *p += 1;
            v |= usize::from(x & 0x7f).checked_shl(shift)?;
            if x & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
        }
    };
    // Each body's ENTRY start (just past its size): hint offsets count from there, locals included.
    let mut entry_starts = Vec::new();
    if let Some(code) = md.section(SectionId::Code) {
        let mut p = code.offset;
        let n = uleb(bytes, &mut p).unwrap_or(0);
        for _ in 0..n {
            let Some(size) = uleb(bytes, &mut p) else { break };
            entry_starts.push(p);
            p += size;
        }
    }
    let first_def = md.imported_func_count() as usize;
    for s in md.sections.iter().filter(|s| s.id == SectionId::Custom) {
        let payload = &bytes[s.offset..s.offset + s.size];
        let mut p = 0;
        let Some(nlen) = uleb(payload, &mut p) else { continue };
        if payload.get(p..p + nlen) != Some(crate::wat::BRANCH_HINT_SECTION) {
            continue;
        }
        p += nlen;
        let funcs = uleb(payload, &mut p).unwrap_or(0);
        for _ in 0..funcs {
            let (Some(f), Some(k)) = (uleb(payload, &mut p), uleb(payload, &mut p)) else {
                out.push(String::from("branch hint section: truncated"));
                return out;
            };
            let def = f.checked_sub(first_def).filter(|&d| d < md.code.len());
            for _ in 0..k {
                let (Some(off), Some(size)) = (uleb(payload, &mut p), uleb(payload, &mut p)) else {
                    out.push(String::from("branch hint section: truncated"));
                    return out;
                };
                p += size;
                let Some(d) = def else {
                    out.push(format!(
                        "@metadata.code.branch_hint annotation: function {f} is not a defined function"
                    ));
                    continue;
                };
                let at = entry_starts.get(d).map(|s| s + off);
                let code = &md.code[d];
                let hits = code.ir.iter().any(|i| {
                    matches!(i.op, Op::If | Op::BrIf)
                        && Some(code.body_offset as usize + i.offset as usize) == at
                });
                if !hits {
                    out.push(format!(
                        "@metadata.code.branch_hint annotation: invalid target (function {f}, offset {off})"
                    ));
                }
            }
        }
    }
    out
}

/// Which rejection stage an assertion demands.
#[derive(Debug, Clone, Copy)]
enum Rejection {
    /// Must fail type-checking.
    Invalid,
    /// Must fail parsing or decoding.
    Malformed,
}

impl Rejection {
    /// Which rejections satisfy this assertion. `is_unsupported` cases are filtered out by
    /// the caller before this is consulted, so anything reaching here is a real verdict on
    /// the module.
    ///
    /// Both kinds accept an **assembler** rejection: for a text module, failing to
    /// assemble *is* the text format rejecting it. wasmrt also resolves statically some
    /// things the spec defers to validation (an unknown `$name`, say), so an
    /// `assert_invalid` module may be turned away a stage earlier than the spec's
    /// pipeline would — still a correct "rejected" outcome.
    fn accepts(self, e: &BuildErr) -> bool {
        match self {
            Rejection::Invalid => matches!(
                e,
                BuildErr::Validate(_) | BuildErr::Decode(_) | BuildErr::Assemble(_)
            ),
            Rejection::Malformed => {
                matches!(e, BuildErr::Decode(_) | BuildErr::Assemble(_))
            }
        }
    }
}

/// Why a module failed to become an instance — the stage matters for the assertions.
enum BuildErr {
    Assemble(wat::Error),
    /// Linking failed with a real **verdict on the module**: it names an import nothing
    /// provides, or provides it as the wrong kind. This is what `assert_unlinkable` asks for.
    Unlinkable(crate::linker::LinkError),
    /// Linking could not be attempted at all, because wasmrt cannot back the kind — an imported
    /// table (T9a#4) or a tag. A SKIP: the module was never put to the test, and scoring a gap
    /// as a pass is how a missing feature comes to look like conformance.
    UnsupportedLink(crate::types::ExternKind),
    Decode(crate::types::DecodeError),
    Validate(crate::validate::ValidateError),
    Instantiate(Trap),
}

impl BuildErr {
    /// Is this "wasmrt cannot express/handle the construct" rather than "the module is
    /// bad"? Those must never be scored as conformance results.
    fn is_unsupported(&self) -> bool {
        matches!(
            self,
            // ⚠️⚠️ `wat::Error::UnknownInstr` was here until 2026-08-19 and it was worth ~300
            // assertions. It meant BOTH "no such instruction exists in any proposal" — a
            // malformed-input verdict wasmrt is entitled to give — and "an instruction we have
            // not implemented", which must never score as a pass. Listing it made every right
            // answer a skip: `load.wast` asserts `i32.load32` is malformed, wasmrt says so, and
            // it scored a SKIP.
            //
            // The assembler now splits them (`wat::classify_unknown_mnemonic`), so only the
            // genuine gap is listed. 🔒 **Do not add `UnknownInstr` back**: the honest way to
            // widen this list is to widen the classifier, where the information is.
            BuildErr::Assemble(
                wat::Error::Unsupported(_)
                    | wat::Error::UnimplementedInstr
                    | wat::Error::NotAModule
            ) | BuildErr::UnsupportedLink(_)
                | BuildErr::Validate(crate::validate::ValidateError::UnsupportedValidation)
                | BuildErr::Instantiate(Trap::UnsupportedImportKind | Trap::UnsupportedInstruction)
        )
    }

    /// Did this module get all the way to **linking** and fail there? That is the outcome
    /// `assert_unlinkable` demands, and it is deliberately narrower than "failed to build":
    /// a module rejected at assembly, decoding or validation was never linked, so counting it
    /// would let a decoder bug masquerade as a conformance pass.
    fn is_link_failure(&self) -> bool {
        matches!(
            self,
            BuildErr::Unlinkable(_)
                | BuildErr::Instantiate(Trap::MissingImport | Trap::IncompatibleImport)
        )
    }
}

impl fmt::Display for BuildErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildErr::Assemble(e) => write!(f, "assemble: {e}"),
            BuildErr::Unlinkable(e) => write!(f, "link: {e}"),
            BuildErr::UnsupportedLink(k) => write!(f, "cannot link an imported {k:?} yet"),
            BuildErr::Decode(e) => write!(f, "decode: {e}"),
            BuildErr::Validate(e) => write!(f, "validate: {e}"),
            BuildErr::Instantiate(t) => write!(f, "instantiate: {t}"),
        }
    }
}

// --- Value literals and matching ----------------------------------------------

/// Render an action or expectation form back to compact text, for a failure message.
///
/// ⚠️ **A failure a reader cannot attribute to an assertion is barely a measurement.** Every
/// `assert_return` failure read `result mismatch (got 0x2, expected …)` with no way to tell WHICH
/// invocation produced it — in a file with 60 assertions that is a bisect, and it is the same
/// lesson the skip census paid for on 2026-08-19 (`best-practices.md` §5.6). One line each.
fn render(form: &Sexpr) -> String {
    match form {
        Sexpr::Atom(a) => a.clone(),
        Sexpr::Str(b) => format!("\"{}\"", String::from_utf8_lossy(b)),
        Sexpr::List(l, _) => {
            let inner: Vec<String> = l.iter().map(render).collect();
            format!("({})", inner.join(" "))
        }
    }
}

/// Parse a concrete argument literal: `(TYPE.const …)` or a reference literal.
fn parse_const(form: &Sexpr) -> Result<Value, String> {
    let l = form
        .as_list()
        .ok_or_else(|| "argument is not a list".to_string())?;
    let kw = l.first().and_then(Sexpr::as_atom).unwrap_or("");
    let lit = l.get(1).and_then(Sexpr::as_atom);
    match kw {
        // `ref.null` carries an ignorable heap type.
        "ref.null" => Ok(NULL_REF),
        "ref.func" => match lit {
            Some(a) => parse_int_lit(a).map(|v| v as Value),
            None => Ok(NULL_REF),
        },
        // ⚠️ `(ref.extern n)` is **not** a bare host index — the spec defines it as
        // `extern.convert_any (ref.host n)`, i.e. an `externref` WRAPPING host address `n`
        // (`extern.wast` asserts exactly that round trip). Passing the bare integer in is what
        // let a host handle be read as a GC heap index, which is why the two conversions were
        // held back until the representation existed.
        "ref.extern" => match lit {
            Some(a) => parse_int_lit(a).map(|v| externalize(host_ref(v as u64))),
            None => Ok(NULL_REF),
        },
        // `(ref.host n)` is the INTERNAL half of the same value: an `anyref` naming host
        // address `n`. It is what `any.convert_extern (ref.extern n)` produces.
        "ref.host" => match lit {
            Some(a) => parse_int_lit(a).map(|v| host_ref(v as u64)),
            None => Ok(NULL_REF),
        },
        "i32.const" => {
            let v = parse_int_lit(lit.ok_or("i32.const: missing literal")?)?;
            Ok(crate::interp::i32_value(v as i32))
        }
        "i64.const" => {
            let v = parse_int_lit(lit.ok_or("i64.const: missing literal")?)?;
            Ok(crate::interp::i64_value(v))
        }
        "f32.const" => {
            let bits = float_bits_32(lit.ok_or("f32.const: missing literal")?)?;
            Ok(Value::from(bits))
        }
        "f64.const" => {
            let bits = float_bits_64(lit.ok_or("f64.const: missing literal")?)?;
            Ok(Value::from(bits))
        }
        "v128.const" => parse_v128(l),
        other => Err(format!("unsupported value literal `{other}`")),
    }
}

/// The interpreter's null-reference sentinel.
const NULL_REF: Value = u64::MAX as Value;

fn parse_int_lit(a: &str) -> Result<i64, String> {
    let t: String = a.chars().filter(|&c| c != '_').collect();
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(&t)),
    };
    let (digits, radix) = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(r) => (r, 16),
        None => (body, 10),
    };
    let mag =
        u64::from_str_radix(digits, radix).map_err(|_| format!("bad integer literal `{a}`"))?;
    Ok(if neg { (mag as i64).wrapping_neg() } else { mag as i64 })
}

/// A float literal's bits, including the wasm NaN spellings. Shares the assembler's parser
/// so an expectation and the module it checks can never disagree about a literal.
fn float_bits_32(lit: &str) -> Result<u32, String> {
    wat::parse_f32_bits(lit, wat::FloatCtx::Script).ok_or_else(|| format!("bad f32 literal `{lit}`"))
}
fn float_bits_64(lit: &str) -> Result<u64, String> {
    wat::parse_f64_bits(lit, wat::FloatCtx::Script).ok_or_else(|| format!("bad f64 literal `{lit}`"))
}

/// `(v128.const <shape> <lane>…)` → the 128-bit value.
fn parse_v128(l: &[Sexpr]) -> Result<Value, String> {
    let shape = l
        .get(1)
        .and_then(Sexpr::as_atom)
        .ok_or("v128.const: missing shape")?;
    let mut bytes = [0u8; 16];
    let lanes: usize = match shape {
        "i8x16" => 16,
        "i16x8" => 8,
        "i32x4" | "f32x4" => 4,
        "i64x2" | "f64x2" => 2,
        _ => return Err(format!("v128.const: bad shape `{shape}`")),
    };
    for k in 0..lanes {
        let a = l
            .get(2 + k)
            .and_then(Sexpr::as_atom)
            .ok_or("v128.const: missing lane")?;
        match shape {
            "i8x16" => bytes[k] = parse_int_lit(a)? as u8,
            "i16x8" => {
                bytes[k * 2..k * 2 + 2].copy_from_slice(&(parse_int_lit(a)? as u16).to_le_bytes());
            }
            "i32x4" => {
                bytes[k * 4..k * 4 + 4].copy_from_slice(&(parse_int_lit(a)? as u32).to_le_bytes());
            }
            "i64x2" => {
                bytes[k * 8..k * 8 + 8].copy_from_slice(&(parse_int_lit(a)? as u64).to_le_bytes());
            }
            "f32x4" => bytes[k * 4..k * 4 + 4].copy_from_slice(&float_bits_32(a)?.to_le_bytes()),
            "f64x2" => bytes[k * 8..k * 8 + 8].copy_from_slice(&float_bits_64(a)?.to_le_bytes()),
            _ => unreachable!(),
        }
    }
    Ok(Value::from_le_bytes(bytes))
}

fn is_canonical_nan_32(bits: u32) -> bool {
    bits & 0x7fff_ffff == 0x7fc0_0000
}
fn is_canonical_nan_64(bits: u64) -> bool {
    bits & 0x7fff_ffff_ffff_ffff == 0x7ff8_0000_0000_0000
}
/// An arithmetic NaN: any NaN whose quiet bit is set.
fn is_arithmetic_nan_32(bits: u32) -> bool {
    bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0 && bits & 0x0040_0000 != 0
}
fn is_arithmetic_nan_64(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000
        && bits & 0x000f_ffff_ffff_ffff != 0
        && bits & 0x0008_0000_0000_0000 != 0
}

fn float_matches_32(got: u32, lit: &str) -> Result<bool, String> {
    match lit {
        "nan:canonical" => return Ok(is_canonical_nan_32(got)),
        "nan:arithmetic" => return Ok(is_arithmetic_nan_32(got)),
        _ => {}
    }
    Ok(got == float_bits_32(lit)?)
}
fn float_matches_64(got: u64, lit: &str) -> Result<bool, String> {
    match lit {
        "nan:canonical" => return Ok(is_canonical_nan_64(got)),
        "nan:arithmetic" => return Ok(is_arithmetic_nan_64(got)),
        _ => {}
    }
    Ok(got == float_bits_64(lit)?)
}

/// Does an actual result match an expected `(TYPE.const …)` form?
fn value_matches(got: Value, exp: &Sexpr) -> Result<bool, String> {
    let l = exp
        .as_list()
        .ok_or_else(|| "expectation is not a list".to_string())?;
    let kw = l.first().and_then(Sexpr::as_atom).unwrap_or("");
    let lit = l.get(1).and_then(Sexpr::as_atom);
    match kw {
        // `(either e1 e2 …)` — the RELAXED-SIMD expectation form. Those instructions have
        // **implementation-defined results** (FMA fusion, NaN propagation, which operand a
        // min/max returns for ±0 or NaN), so the suite lists every answer the spec permits and
        // any one of them is a pass.
        //
        // ⚠️ This is not leniency: outside `either` the comparison stays exact, and an engine
        // that returned something on *neither* list would still fail. Missing it cost **38
        // assertions across five files** — `relaxed_min_max`, `relaxed_madd_nmadd`,
        // `relaxed_laneselect`, `simd_f32x4_rounding`, `simd_f64x2_rounding` — all reported as
        // `unsupported value literal 'either'`, which named the harness, not the engine.
        "either" => {
            for alt in &l[1..] {
                if value_matches(got, alt)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        "ref.null" => return Ok(got == NULL_REF),
        // A bare `(ref.func)` / `(ref.extern)` asserts merely non-null; with a payload it
        // is exact. The abstract GC matchers assert non-null of that kind, which the
        // interpreter's untyped slot cannot distinguish — non-null is the honest check.
        // `(ref.func N)` names WHICH FUNCTION, so compare the function index — a funcref value also
        // carries its owning instance in the high bits, and for any module past the first that would
        // never equal the bare literal. `ref.extern N` is a host index with no such packing, so the
        // two spellings no longer share their comparison.
        "ref.func" => {
            return match lit {
                Some(a) => Ok((got as u32) == parse_int_lit(a)? as u32),
                None => Ok(got != NULL_REF),
            };
        }
        "ref.extern" => {
            return match lit {
                Some(a) => Ok(got == externalize(host_ref(parse_int_lit(a)? as u64))),
                None => Ok(got != NULL_REF),
            };
        }
        // `(ref.host n)` names WHICH host address, and the internal (unwrapped) form of it —
        // `externalize` is deliberately absent here, so `internalize`/`externalize` cannot both
        // be no-ops and still satisfy this file.
        "ref.host" => {
            return match lit {
                Some(a) => Ok(got == host_ref(parse_int_lit(a)? as u64)),
                None => Ok(got != NULL_REF),
            };
        }
        "ref.struct" | "ref.array" | "ref.i31" | "ref.eq" | "ref.any" | "ref.data" => {
            return Ok(got != NULL_REF)
        }
        "f32.const" => {
            return float_matches_32(got as u32, lit.ok_or("f32.const: missing literal")?);
        }
        "f64.const" => {
            return float_matches_64(got as u64, lit.ok_or("f64.const: missing literal")?);
        }
        "v128.const" => {
            // Float lanes are matched lane by lane, so a per-lane `nan:canonical` works.
            let shape = l.get(1).and_then(Sexpr::as_atom).unwrap_or("");
            if shape == "f32x4" || shape == "f64x2" {
                let bytes = got.to_le_bytes();
                let (lanes, width) = if shape == "f32x4" { (4, 4) } else { (2, 8) };
                for k in 0..lanes {
                    let a = l
                        .get(2 + k)
                        .and_then(Sexpr::as_atom)
                        .ok_or("v128.const: missing lane")?;
                    let ok = if width == 4 {
                        let mut b = [0u8; 4];
                        b.copy_from_slice(&bytes[k * 4..k * 4 + 4]);
                        float_matches_32(u32::from_le_bytes(b), a)?
                    } else {
                        let mut b = [0u8; 8];
                        b.copy_from_slice(&bytes[k * 8..k * 8 + 8]);
                        float_matches_64(u64::from_le_bytes(b), a)?
                    };
                    if !ok {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            return Ok(got == parse_v128(l)?);
        }
        _ => {}
    }
    // Integers and everything else: exact comparison against the parsed literal.
    Ok(got == parse_const(exp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Summary {
        run_script(src.as_bytes()).expect("script parse failed")
    }

    #[test]
    fn runs_a_simple_assert_return() {
        let s = run(
            r#"(module (func (export "add") (param i32 i32) (result i32)
                 (i32.add (local.get 0) (local.get 1))))
               (assert_return (invoke "add" (i32.const 40) (i32.const 2)) (i32.const 42))"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (1, 0, 0));
    }

    // The shared store gives every instance a slot in one global pool, so an instruction that
    // indexes a pool with its raw module-local immediate silently reads the *first* module's
    // resource. With one instance per store the two indices coincide and the bug is invisible;
    // these tests keep a second instance around so they cannot coincide.

    #[test]
    fn call_indirect_uses_the_callers_own_table() {
        let s = run(
            r#"(module (table 1 funcref) (elem (i32.const 0) $a) (func $a (result i32) (i32.const 11)))
               (module (type $t (func (result i32)))
                 (table 1 funcref) (elem (i32.const 0) $b) (func $b (result i32) (i32.const 22))
                 (func (export "f") (result i32) (call_indirect (type $t) (i32.const 0))))
               (assert_return (invoke "f") (i32.const 22))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn memory_init_reads_the_instances_own_data_segment() {
        let s = run(
            r#"(module (memory 1) (data "\aa\aa\aa\aa"))
               (module (memory 1) (data $d "\01\02\03\04")
                 (func (export "f") (result i32)
                   (memory.init $d (i32.const 0) (i32.const 0) (i32.const 4))
                   (i32.load (i32.const 0))))
               (assert_return (invoke "f") (i32.const 0x04030201))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn a_failed_builds_assertions_do_not_run_against_an_earlier_module() {
        // T9a#3. The first module builds and is named, so it lives in `named`. The second
        // fails to build — one failure. Its assertion must be SKIPPED ("nothing was
        // tested"), not redirected to `$m`, which would answer 1 and report a value
        // mismatch: a phantom defect pointing at code that is correct.
        let s = run(
            r#"(module $m (func (export "f") (result i32) (i32.const 1)))
               (module (func (export "f") (result i32) (i64.const 2)))
               (assert_return (invoke "f") (i32.const 2))"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (0, 1, 1), "{:?}", s.failures);
        assert!(s.failures[0].contains("failed to build"), "{:?}", s.failures);
    }

    #[test]
    fn a_named_module_still_takes_bare_actions_when_nothing_failed() {
        // The other half of the same fix: the fall-back itself is wanted. A file that names
        // every module must still run its un-named actions.
        let s = run(
            r#"(module $m (func (export "f") (result i32) (i32.const 7)))
               (assert_return (invoke "f") (i32.const 7))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn reports_a_mismatch_as_a_failure() {
        let s = run(
            r#"(module (func (export "f") (result i32) (i32.const 1)))
               (assert_return (invoke "f") (i32.const 2))"#,
        );
        assert_eq!(s.failed, 1);
        assert_eq!(s.passed, 0);
        assert!(s.failures[0].contains("mismatch"));
    }

    #[test]
    fn checks_arity() {
        let s = run(
            r#"(module (func (export "f") (result i32) (i32.const 1)))
               (assert_return (invoke "f") (i32.const 1) (i32.const 2))"#,
        );
        assert_eq!(s.failed, 1);
        assert!(s.failures[0].contains("arity"));
    }

    #[test]
    fn runs_assert_trap() {
        let s = run(
            r#"(module (func (export "boom") (result i32)
                 (i32.div_s (i32.const 1) (i32.const 0))))
               (assert_trap (invoke "boom") "integer divide by zero")"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0));
    }

    #[test]
    fn a_missing_trap_is_a_failure() {
        let s = run(
            r#"(module (func (export "ok") (result i32) (i32.const 1)))
               (assert_trap (invoke "ok") "integer divide by zero")"#,
        );
        assert_eq!(s.failed, 1);
    }

    #[test]
    fn runs_assert_invalid() {
        // A type error the validator must catch.
        let s = run(
            r#"(assert_invalid
                 (module (func (result i32) (f32.const 0)))
                 "type mismatch")"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0));
    }

    #[test]
    fn an_accepted_module_fails_assert_invalid() {
        let s = run(
            r#"(assert_invalid
                 (module (func (result i32) (i32.const 0)))
                 "type mismatch")"#,
        );
        assert_eq!(s.failed, 1);
        assert!(s.failures[0].contains("should be rejected"));
    }

    /// `spectest.shared_memory` exists, and the `shared` flag is what decides compatibility.
    ///
    /// ⚠️⚠️ **The middle assertion here was already PASSING while the export did not exist.**
    /// An unresolvable import is a link failure, and `assert_unlinkable` asks only that linking
    /// fail — so it was right for the wrong reason, and could not have caught a linker that
    /// ignored `shared`. *A blanket absence is not a neutral placeholder*, the same lesson the
    /// blanket `assert_unlinkable` skip taught at T9a#4.
    #[test]
    fn spectest_shared_memory_links_and_sharedness_is_enforced() {
        let s = run(
            r#"(module (import "spectest" "shared_memory" (memory 1 2 shared)))
               (assert_unlinkable
                 (module (import "spectest" "shared_memory" (memory 1 2)))
                 "incompatible import type")
               (assert_unlinkable
                 (module (import "spectest" "memory" (memory 1 2 shared)))
                 "incompatible import type")"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (2, 0, 0), "{:?}", s.failures);
    }

    /// A validity assertion must be adjudicated at **validation**, never carried on to link.
    ///
    /// ⚠️⚠️ **The module here is VALID and its import is unresolvable, which is the exact pair
    /// that produced a misreport.** Running the full build pipeline made it fail at link, and
    /// the runner printed *"rejected at the wrong stage (link: unknown import)"* — a sentence
    /// about the engine that was really about the runner. The honest verdict is *"module was
    /// accepted"*: it validated, so the assertion is wrong about it.
    ///
    /// Four of `proposals/threads/imports.wast`'s failures read that way until 2026-09-17, and
    /// the shape is corpus-wide, not a threads matter.
    #[test]
    fn a_validity_assertion_stops_at_validation_and_never_links() {
        let s = run(
            r#"(assert_invalid
                 (module (import "spectest" "nothing-provides-this" (func)))
                 "some reason")"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (0, 1, 0));
        assert!(
            s.failures[0].contains("module was accepted"),
            "an unresolvable import must not be reported as a rejection stage: {}",
            s.failures[0]
        );
        assert!(
            !s.failures[0].contains("wrong stage"),
            "the runner must not take a validity assertion to the linker: {}",
            s.failures[0]
        );
    }

    #[test]
    fn an_assembler_gap_is_skipped_not_passed() {
        // THE honesty property: a construct the assembler cannot express must not satisfy
        // an `assert_invalid`, or missing features would masquerade as conformance.
        //
        // 🔻 **The EXAMPLE moved on 2026-08-19; the property did not.** It used to be
        // `i32.nonexistent_opcode`, chosen to stand for "a mnemonic our assembler does not
        // know". After the `UnknownInstr` split that name is no longer an instance of the
        // property at all — it exists in **no** proposal, so refusing it is a *verdict*, not a
        // gap. **A test that fails because its example was reclassified is STALE, not broken:**
        // keep the property, re-pick the example. The example must now be a mnemonic that is a
        // real instruction wasmrt has not built.
        let s = run(
            r#"(assert_invalid
                 (module (func (struct.new_desc)))
                 "some reason")"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (0, 0, 1), "our gap must SKIP");
    }

    /// `(module definition $M …)` + `(module instance $I $M)` — **instantiation is generative**,
    /// so two instances of one definition must have SEPARATE state.
    ///
    /// ⚠️ This is the property the feature exists for, and the one a lazy implementation gets
    /// wrong: instantiating the definition once and handing the same instance to every
    /// `(module instance …)` would satisfy every *shape* check in `instance.wast` and fail this.
    /// The mutation is not hypothetical — it is the obvious way to write it.
    #[test]
    fn module_instances_of_one_definition_have_separate_state() {
        let s = run(
            r#"
            (module definition $M
              (global (export "g") (mut i32) (i32.const 0))
              (func (export "bump") (result i32)
                (global.set 0 (i32.add (global.get 0) (i32.const 1)))
                (global.get 0)))
            (module instance $A $M)
            (module instance $B $M)
            (assert_return (invoke $A "bump") (i32.const 1))
            (assert_return (invoke $A "bump") (i32.const 2))
            ;; $B must start from zero — if it shared $A's globals it would return 3.
            (assert_return (invoke $B "bump") (i32.const 1))
            "#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (3, 0, 0), "{:?}", s.failures);
    }

    /// `(register "name")` with no `$id` registers the **most recent** module — including a
    /// NAMED one.
    ///
    /// ⚠️ It used to fall back to `current`, which is deliberately `None` after a named module,
    /// so `(module $M …) (register "M")` registered **nothing** and every later import of `"M"`
    /// failed to link — one failed build plus every assertion behind it. 🎓 `target()` next door
    /// had carried the correct fallback all along: *a rule one call site does not consult is a
    /// rule with an exception nobody wrote down.*
    #[test]
    fn register_with_no_id_takes_the_most_recent_module_even_if_named() {
        let s = run(
            r#"
            (module $M (memory (export "mem") 1)
              (func (export "read") (param i32) (result i32) (i32.load8_u (local.get 0))))
            (register "M")
            (module
              (memory $m (import "M" "mem") 1)
              (func (export "write") (i32.store8 (i32.const 0) (i32.const 7))))
            (invoke "write")
            (assert_return (invoke $M "read" (i32.const 0)) (i32.const 7))
            "#,
        );
        // The import must LINK and the two modules must share the memory.
        assert_eq!((s.passed, s.failed, s.skipped), (3, 0, 0), "{:?}", s.failures);
    }

    /// 🔒 The inverse, pinned beside it — without this, "skip everything we cannot assemble"
    /// would pass the test above and the ~300 assertions the split recovered would be lost
    /// again.
    #[test]
    fn a_mnemonic_in_no_proposal_is_a_verdict_not_a_gap() {
        // `i32.load32` is not an instruction in any WebAssembly proposal — `load.wast` asserts
        // exactly this, and being unknown IS the malformation under test.
        let s = run(
            r#"(assert_malformed
                 (module quote "(func (i32.load32 (i32.const 0)))")
                 "unknown operator")"#,
        );
        assert_eq!(
            (s.passed, s.failed, s.skipped),
            (1, 0, 0),
            "a mnemonic that exists nowhere must PASS an assert_malformed, not skip"
        );
    }

    #[test]
    fn runs_assert_malformed_on_a_bad_binary() {
        let s = run(
            r#"(assert_malformed
                 (module binary "\00asm\01\00\00\00\01\00")
                 "unexpected end")"#,
        );
        // A truncated section must be rejected at decode.
        assert_eq!(s.failed, 0);
        assert!(s.passed + s.skipped == 1);
    }

    #[test]
    fn matches_float_results_including_nan_forms() {
        let s = run(
            r#"(module
                 (func (export "half") (result f64) (f64.const 0.5))
                 (func (export "nan") (result f64)
                   (f64.div (f64.const 0) (f64.const 0))))
               (assert_return (invoke "half") (f64.const 0.5))
               (assert_return (invoke "nan") (f64.const nan:canonical))"#,
        );
        assert_eq!((s.passed, s.failed), (2, 0), "{:?}", s.failures);
    }

    #[test]
    fn matches_a_v128_result_as_one_slot() {
        // wasmrt's 128-bit value slot means a v128 result is ONE slot, so the arity check
        // compares directly against the expectation count.
        let s = run(
            r#"(module (func (export "v") (result v128)
                 (i32x4.splat (i32.const 7))))
               (assert_return (invoke "v") (v128.const i32x4 7 7 7 7))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn runs_named_modules() {
        let s = run(
            r#"(module $A (func (export "f") (result i32) (i32.const 1)))
               (module $B (func (export "f") (result i32) (i32.const 2)))
               (assert_return (invoke $A "f") (i32.const 1))
               (assert_return (invoke $B "f") (i32.const 2))"#,
        );
        assert_eq!((s.passed, s.failed), (2, 0), "{:?}", s.failures);
    }

    #[test]
    fn runs_a_quoted_module() {
        let s = run(
            r#"(module quote "(func (export \"f\") (result i32) (i32.const 9))")
               (assert_return (invoke "f") (i32.const 9))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    /// ⚠️⚠️ **Every skip must carry a reason.** A skip site that only bumps the counter is
    /// invisible in the report, and this project spent the whole port unable to say what its
    /// 1,024 skips *were* — the census that finally answered it showed **91% were cascades**
    /// behind a handful of roots, not 1,024 pieces of work. A bare counter is not a
    /// measurement, so the invariant is pinned rather than trusted.
    ///
    /// The script below deliberately trips several DIFFERENT skip paths in one run: an
    /// unhandled command, a module that will not assemble, and the actions stranded behind it.
    #[test]
    fn every_skip_records_a_reason() {
        let s = run_script(
            // ⚠️ The unbuildable module must use a construct `wat::classify_unknown_mnemonic`
            // still reports as OUR gap. It was `any.convert_extern` until that landed (S1), then
            // `i64.add128` until Track W landed it (2026-09-17), and is `struct.new_desc`
            // (custom-descriptors, Track D) now. **Swap it the day Track D lands** — and when
            // nothing is left unimplemented, delete this arm rather than fake it: at that point
            // there is no "module: unsupported" skip left to record a reason for.
            //
            // 🎓 Third rotation of the same example. *A test that fails because its example was
            // reclassified is STALE, not broken* — and this one names its own successor, which is
            // why the rotation costs a line instead of a debugging session.
            br#"(assert_flurb (invoke "nope"))
                (module (func (export "f") (result i64)
                  (struct.new_desc (i64.const 1) (i64.const 0) (i64.const 1) (i64.const 0))))
                (assert_return (invoke "f") (i64.const 1))
                (assert_trap (invoke "f") "x")
                (invoke "f")"#,
        )
        .unwrap();
        assert!(s.skipped > 0, "the fixture must actually skip: {s}");
        assert_eq!(
            s.skips.len(),
            s.skipped,
            "a skip without a reason is a hole in the report: {:?}",
            s.skips
        );
        // …and the reasons must DISTINGUISH the paths. One catch-all string would satisfy the
        // count above while telling a reader nothing, which is the failure mode being pinned.
        assert!(
            s.skips.iter().any(|r| r.contains("unhandled command")),
            "{:?}",
            s.skips
        );
        assert!(
            s.skips.iter().any(|r| r.starts_with("module: unsupported")),
            "{:?}",
            s.skips
        );
        assert!(
            s.skips.iter().any(|r| r.contains("no target instance")),
            "{:?}",
            s.skips
        );
    }

    #[test]
    fn assert_exhaustion_needs_real_exhaustion() {
        // Runs on a thread with a large stack. The interpreter's 512-frame recursion cap
        // matches the oracle's, but a DEBUG-profile `run` frame is big enough that the
        // native stack can go first — release is fine. See `cmem/known-issues.md`; the cap
        // is deliberately left at the oracle's value rather than tuned to one profile.
        let h = std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                run_script(
                    br#"(module (func $f (export "f") (result i32) (call $f)))
                        (assert_exhaustion (invoke "f") "call stack exhausted")"#,
                )
                .unwrap()
            })
            .unwrap();
        let s = h.join().unwrap();
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
    }

    #[test]
    fn unknown_commands_are_skipped() {
        let s = run(r#"(assert_flurb (invoke "f"))"#);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.passed + s.failed, 0);
    }

    /// `assert_exception` accepts ONE outcome: an exception nothing caught. A plain trap is not
    /// it — scoring "the call ended" as a pass would let a handler bug hide behind an unrelated
    /// trap, and the same exports carry both kinds of assertion in `try_table.wast`.
    #[test]
    fn assert_exception_distinguishes_an_exception_from_a_trap() {
        let s = run(
            r#"(module
                 (tag $e)
                 (func (export "throws") (throw $e))
                 (func (export "traps") (unreachable))
                 (func (export "caught") (block $h (try_table (catch_all $h) (throw $e)))))
               (assert_exception (invoke "throws"))"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);

        for bad in ["traps", "caught"] {
            let s = run(&format!(
                r#"(module
                     (tag $e)
                     (func (export "throws") (throw $e))
                     (func (export "traps") (unreachable))
                     (func (export "caught") (block $h (try_table (catch_all $h) (throw $e)))))
                   (assert_exception (invoke "{bad}"))"#
            ));
            assert_eq!((s.passed, s.failed), (0, 1), "{bad} must not satisfy it");
        }
    }

    /// `(module quote "(module …)")` — the quoted text may be a WHOLE module, not only its
    /// fields. Until 2026-09-19 the runner wrapped it in a second `(module …)` regardless, so a
    /// valid module failed to build and — the half that mattered — every `assert_malformed` of
    /// that shape passed on the wrapper's `BadModuleField` without reaching its rule.
    #[test]
    fn a_quoted_whole_module_is_not_wrapped_twice() {
        let s = run(
            r#"(module quote "(module (func (export \"f\") (result i32) (i32.const 7)))")
               (assert_return (invoke "f") (i32.const 7))
               (module quote "(func (export \"g\") (result i32) (i32.const 8))")
               (assert_return (invoke "g") (i32.const 8))"#,
        );
        assert_eq!((s.passed, s.failed, s.skipped), (2, 0, 0), "{:?}", s.failures);

        // The load-bearing direction: a VALID whole-module quote must now fail an
        // `assert_malformed`. Under the old wrapper this scored a pass.
        let s = run(r#"(assert_malformed (module quote "(module (func))") "anything")"#);
        assert_eq!((s.passed, s.failed), (0, 1), "a wrapper refusal must not score as a pass");
    }

    /// `assert_malformed_custom` passes on the ASSERTED annotation rule only — not on some
    /// other annotation rule, not on any other refusal, and not on acceptance.
    #[test]
    fn assert_malformed_custom_needs_the_asserted_rule() {
        let s = run(
            r#"(assert_malformed_custom (module quote "(@custom)") "@custom annotation: missing section name")"#,
        );
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
        for bad in [
            // wrong annotation rule
            r#"(assert_malformed_custom (module quote "(@custom)") "@custom annotation: malformed placement")"#,
            // refused, but not for an annotation
            r#"(assert_malformed_custom (module quote "(func (nop nop))") "@custom annotation: missing section name")"#,
            // accepted
            r#"(assert_malformed_custom (module quote "(@custom \"x\")") "@custom annotation: missing section name")"#,
        ] {
            let s = run(bad);
            assert_eq!((s.passed, s.failed), (0, 1), "{bad} must fail: {:?}", s.failures);
        }
    }

    /// `assert_invalid_custom`: the module is valid AND a custom section is not. A hint on a
    /// non-branch instruction is the case: emitted (as wasm-tools emits it), then reported.
    #[test]
    fn assert_invalid_custom_needs_a_valid_module_with_a_bad_hint() {
        let good = r#"(assert_invalid_custom
            (module (func (param i32) (result i32)
              (local.get 0) (@metadata.code.branch_hint "\01") (i32.eqz)))
            "@metadata.code.branch_hint annotation: invalid target")"#;
        let s = run(good);
        assert_eq!((s.passed, s.failed), (1, 0), "{:?}", s.failures);
        // A hint on a real branch is sound, so the assertion must fail.
        let s = run(
            r#"(assert_invalid_custom
                (module (func (@metadata.code.branch_hint "\01") (if (i32.const 0) (then))))
                "@metadata.code.branch_hint annotation: invalid target")"#,
        );
        assert_eq!((s.passed, s.failed), (0, 1), "{:?}", s.failures);
    }
}
