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

use crate::interp::{HostFunc, Imports, InstanceId, Store, Trap, Value};
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

/// Parse and run a whole `.wast` source.
///
/// # Errors
/// Returns [`Error::Parse`] if the source is not well-formed s-expressions. Command-level
/// problems are counted in the [`Summary`] rather than returned.
pub fn run_script(src: &[u8]) -> Result<Summary, Error> {
    let forms = sexpr::parse_all(src)?;
    let mut r = Runner::default();
    for cmd in &forms {
        r.command(cmd);
    }
    Ok(r.summary)
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

/// The spec suite's standard `spectest` host module.
///
/// Its `print*` functions exist only to be callable — the suite asserts nothing about what
/// they emit — and its globals have values the suite checks, so those must be exact.
fn spectest_func(_name: &str) -> HostFunc {
    // Every `print*` variant takes its arguments and returns nothing.
    HostFunc::new(|_caller, _args, _results| Ok(()))
}

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

    fn command(&mut self, cmd: &Sexpr) {
        let Some(kw) = cmd.keyword() else {
            self.summary.skipped += 1;
            return;
        };
        let list = cmd.as_list().unwrap_or(&[]);
        match kw {
            "module" => self.define_module(list),
            "assert_return" => self.assert_return(list),
            "assert_trap" => self.assert_trap(list),
            "assert_exhaustion" => self.assert_exhaustion(list),
            "assert_invalid" => self.assert_rejected(list, Rejection::Invalid),
            "assert_malformed" => self.assert_rejected(list, Rejection::Malformed),
            "assert_unlinkable" => self.assert_unlinkable(list),
            "invoke" | "get" => match self.run_action(cmd) {
                Ok(_) => self.summary.passed += 1,
                Err(ActionErr::NoTarget) => self.summary.skipped += 1,
                Err(ActionErr::Trap(t)) => self.fail(format!("action trapped: {t}")),
                Err(ActionErr::Bad(m)) => self.fail(m),
            },
            // `(register "name" $id?)` publishes a module's exports under `name`, so later
            // modules can import from it: the $id-named module if given, else the current one.
            "register" => {
                let target = list
                    .get(2)
                    .and_then(Sexpr::as_atom)
                    .filter(|a| a.starts_with('$'))
                    .and_then(|a| self.named.iter().find(|(n, _)| n == a).map(|(_, i)| *i))
                    .or(self.current);
                match (list.get(1).and_then(Sexpr::as_str), target) {
                    (Some(name), Some(id)) => {
                        let name = String::from_utf8_lossy(name).into_owned();
                        self.registered.push((name, id));
                        self.summary.passed += 1;
                    }
                    _ => self.summary.skipped += 1,
                }
            }
            _ => self.summary.skipped += 1,
        }
    }

    /// Assemble a `(module …)` form to bytes. Handles the `binary` and `quote` variants.
    fn module_binary(form: &[Sexpr]) -> Result<Vec<u8>, wat::Error> {
        // `(module quote "…" …)` — the strings are `.wat` source holding the module's
        // FIELDS, not a whole `(module …)` form, so they are wrapped before assembly.
        let quote_at = form.iter().position(|s| s.as_atom() == Some("quote"));
        if let Some(q) = quote_at {
            let mut src = b"(module\n".to_vec();
            for s in &form[q + 1..] {
                src.extend_from_slice(s.as_str().unwrap_or(&[]));
                src.push(b'\n');
            }
            src.extend_from_slice(b")\n");
            return wat::assemble(&src);
        }
        wat::assemble_module(form)
    }

    /// Build the [`Linker`] this script links against: the standard `spectest` host module
    /// plus every module published by `(register …)`.
    ///
    /// Rebuilt per module rather than kept on the `Runner`, because `(register …)` can add
    /// a namespace between two builds and the linker must see it.
    fn linker(&self) -> Linker {
        let mut l = Linker::new();
        l.define_namespace("spectest", spectest_func);
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
            l.define_table("spectest", "table", id, 0);
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
            b"(module (memory (export \"memory\") 1 2) (table (export \"table\") 10 20 funcref))",
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

    fn build(&mut self, form: &[Sexpr]) -> Result<InstanceId, BuildErr> {
        let bytes = Self::module_binary(form).map_err(BuildErr::Assemble)?;
        self.instantiate_bytes(&bytes)
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
    fn try_module_definition_or_instance(&mut self, list: &[Sexpr]) -> bool {
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
            let mut form: Vec<Sexpr> = alloc::vec![Sexpr::Atom(String::from("module"))];
            form.extend(list[k + 1 + usize::from(name.is_some())..].iter().cloned());
            match crate::wat::assemble_module(&form) {
                Ok(bytes) => {
                    if let Some(n) = name {
                        self.definitions.push((n, bytes));
                    }
                }
                Err(e) if BuildErr::Assemble(e.clone()).is_unsupported() => {
                    self.summary.skipped += 1;
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
            self.summary.skipped += 1;
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
                    self.summary.skipped += 1;
                } else {
                    self.fail(format!("module instance failed to build: {e}"));
                }
                self.current = None;
                self.last_build_failed = true;
            }
        }
        true
    }

    fn define_module(&mut self, list: &[Sexpr]) {
        if self.try_module_definition_or_instance(list) {
            return;
        }
        self.last_build_failed = false;
        match self.build(list) {
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
                    self.summary.skipped += 1;
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
            // Reading an exported global is not part of the embedding surface yet.
            return Err(ActionErr::NoTarget);
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
                self.summary.skipped += 1;
                return;
            }
            Err(ActionErr::Trap(t)) => {
                self.fail(format!("assert_return: unexpected trap {t}"));
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
                "assert_return: arity {} != expected {}",
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
                        "assert_return: result mismatch (got 0x{got:x}, expected {exp:?})"
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
            match self.build(operand.as_list().unwrap_or(&[])) {
                Ok(_) => self.fail("assert_trap: module instantiated without trapping".to_string()),
                Err(BuildErr::Instantiate(_)) => self.summary.passed += 1,
                Err(e) if e.is_unsupported() => self.summary.skipped += 1,
                Err(e) => self.fail(format!("assert_trap: non-trap error {e}")),
            }
            return;
        }
        match self.run_action(operand) {
            Ok(_) => self.fail("assert_trap: expected a trap, got a result".to_string()),
            Err(ActionErr::Trap(_)) => self.summary.passed += 1,
            Err(ActionErr::NoTarget) => self.summary.skipped += 1,
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
            Err(ActionErr::NoTarget) => self.summary.skipped += 1,
            Err(ActionErr::Bad(m)) => self.fail(m),
        }
    }

    /// `assert_invalid` / `assert_malformed (module …) "reason"` — the module must be
    /// rejected, and by the **right stage**.
    fn assert_rejected(&mut self, form: &[Sexpr], kind: Rejection) {
        let Some(inner) = form.get(1).filter(|s| s.keyword() == Some("module")) else {
            self.summary.skipped += 1;
            return;
        };
        match self.build(inner.as_list().unwrap_or(&[])) {
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
                    self.summary.skipped += 1;
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
            self.summary.skipped += 1;
            return;
        };
        match self.build(inner.as_list().unwrap_or(&[])) {
            Ok(_) => self.fail(format!(
                "Unlinkable: module linked (should fail to link: {})",
                match form.get(2) {
                    Some(Sexpr::Str(b)) => String::from_utf8_lossy(b).into_owned(),
                    Some(Sexpr::Atom(a)) => a.clone(),
                    _ => String::from("<no reason given>"),
                }
            )),
            Err(e) if e.is_unsupported() => self.summary.skipped += 1,
            Err(e) if e.is_link_failure() => self.summary.passed += 1,
            Err(e) => self.fail(format!("Unlinkable: rejected before linking ({e})")),
        }
    }
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
        "ref.func" | "ref.extern" => match lit {
            Some(a) => parse_int_lit(a).map(|v| v as Value),
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
    wat::parse_f32_bits(lit).ok_or_else(|| format!("bad f32 literal `{lit}`"))
}
fn float_bits_64(lit: &str) -> Result<u64, String> {
    wat::parse_f64_bits(lit).ok_or_else(|| format!("bad f64 literal `{lit}`"))
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
                Some(a) => Ok(got == parse_int_lit(a)? as Value),
                None => Ok(got != NULL_REF),
            };
        }
        "ref.struct" | "ref.array" | "ref.i31" | "ref.eq" | "ref.any" | "ref.host"
        | "ref.data" => return Ok(got != NULL_REF),
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
                 (module (func (i64.add128)))
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
        let s = run(r#"(assert_exception (invoke "f"))"#);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.passed + s.failed, 0);
    }
}
