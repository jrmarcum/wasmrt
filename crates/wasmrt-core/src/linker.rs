//! Name-based import resolution — the layer between "here are some backings" and a
//! module's declared import list.
//!
//! [`crate::interp::Imports`] is **positional**: its vectors align with the module's
//! imports *in declaration order*. That is the right shape for the engine (it is what
//! instantiation consumes, with no lookup on any hot path) and the wrong shape for an
//! embedder, who knows names — `"wasi_snapshot_preview1"`, `"env"`, `"spectest"` — and
//! has no reason to know what order a guest happened to declare them in.
//!
//! A [`Linker`] holds definitions keyed by `(module, name)` and walks a module's imports to
//! produce the positional `Imports` the engine wants. Getting that walk wrong is silent:
//! bind two same-kind imports in the wrong order and both still link, but each call reaches
//! the other's backing. Doing it in exactly one place is the point of this module.
//!
//! **Owner decision (2026-08-06):** this lives in `wasmrt-core`, not in the C ABI crate, so
//! the C ABI, the native Rust crate, WASI and the `.wast` runner all resolve imports through
//! one authority instead of three that can drift.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::interp::{Caller, HostFunc, Imports, InstanceId, Result as TrapResult, Store, Value};
use crate::module::Module;
use crate::types::ExternKind;

/// The callback shape a linker stores for a host function. Held behind an [`Rc`] so one
/// `Linker` can satisfy many modules — resolving must not consume the definition.
type LinkedFn = dyn Fn(&mut Caller<'_>, &[Value], &mut [Value]) -> TrapResult<()>;

/// A factory that produces a backing for *any* name in a namespace. This is how a large
/// host module is defined without enumerating it: WASI preview 1 routes ~45 calls by name,
/// and the spec suite's `spectest` accepts whatever a test asks for.
type NamespaceFn = dyn Fn(&str) -> HostFunc;

/// A factory of last resort, receiving `(module, name)` for an import nothing else matched.
type FallbackFn = dyn Fn(&str, &str) -> HostFunc;

/// Why linking a module failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The module imports `module::name` and nothing defines it.
    UnknownImport { module: String, name: String },
    /// The import resolved to a definition of the wrong kind (a global where a function was
    /// declared, say).
    KindMismatch {
        module: String,
        name: String,
        expected: ExternKind,
    },
    /// The module imports a memory, table or tag. Instances can share memories and tables
    /// through the store, but there is no way yet to *name* one as a linker definition.
    UnsupportedImportKind(ExternKind),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::UnknownImport { module, name } => {
                write!(f, "unknown import: `{module}`.`{name}` is not defined")
            }
            LinkError::KindMismatch {
                module,
                name,
                expected,
            } => write!(
                f,
                "import `{module}`.`{name}` is declared as {expected:?}, but the definition is a different kind"
            ),
            LinkError::UnsupportedImportKind(k) => {
                write!(f, "cannot link an imported {k:?} by name")
            }
        }
    }
}

impl core::error::Error for LinkError {}

/// One named definition.
enum Def {
    Func(Rc<LinkedFn>),
    Global(Value),
    /// Every export of an already-instantiated module, published under one namespace —
    /// what the `.wast` `(register "name")` command does.
    Instance(InstanceId),
    /// A catch-all for a whole namespace: any name resolves, the factory deciding what it
    /// does (including "return NOSYS").
    Namespace(Rc<NamespaceFn>),
}

/// A registry of import backings, resolved against a module's declared imports **by name**.
///
/// ```ignore
/// let mut linker = Linker::new();
/// linker.define_func("env", "add", |_caller, args, results| {
///     results[0] = i32_value(as_i32(args[0]) + as_i32(args[1]));
///     Ok(())
/// });
/// let id = linker.instantiate(&mut store, module)?;
/// ```
///
/// A `Linker` is reusable: resolving borrows its definitions rather than consuming them, so
/// one linker can instantiate many modules — which is the normal embedder shape (define the
/// host surface once, run several guests against it).
#[derive(Default)]
pub struct Linker {
    /// `(module, name, def)`. A `Vec` rather than a map: an embedder defines a handful of
    /// names, resolution happens once per instantiation and never on a hot path, and this
    /// keeps insertion order meaningful for the last-definition-wins rule below.
    defs: Vec<(String, String, Def)>,
    /// Namespace-level catch-alls, consulted only when no exact `(module, name)` matches.
    namespaces: Vec<(String, Def)>,
    /// Consulted only when neither an exact definition nor a namespace matches.
    fallback: Option<Rc<FallbackFn>>,
}

impl Linker {
    /// An empty linker.
    #[must_use]
    pub fn new() -> Linker {
        Linker::default()
    }

    /// Define a host function under `module`.`name`.
    ///
    /// The callback receives the guest's arguments and a results slice already sized to the
    /// import's declared arity; returning `Err` traps the guest. Redefining a name replaces
    /// the previous definition.
    pub fn define_func(
        &mut self,
        module: &str,
        name: &str,
        f: impl Fn(&mut Caller<'_>, &[Value], &mut [Value]) -> TrapResult<()> + 'static,
    ) {
        self.insert(module, name, Def::Func(Rc::new(f)));
    }

    /// Define a global's value under `module`.`name`. Passed to the importer by value — see
    /// [`Store::export_global`] for why.
    pub fn define_global(&mut self, module: &str, name: &str, value: Value) {
        self.insert(module, name, Def::Global(value));
    }

    /// Publish every export of an already-instantiated module under the namespace `module`,
    /// so later modules can import from it. This is wasm→wasm linking: the callee runs
    /// against **its own** instance, so it sees the exporter's memory and globals.
    pub fn define_instance(&mut self, module: &str, instance: InstanceId) {
        self.namespaces
            .retain(|(m, _)| m.as_str() != module);
        self.namespaces
            .push((String::from(module), Def::Instance(instance)));
    }

    /// Define a whole namespace at once: any name in `module` resolves, `factory` deciding
    /// what backs it.
    ///
    /// This exists because the alternative — enumerating every name — is where host modules
    /// rot. WASI preview 1 dispatches ~45 calls by name and answers `NOSYS` for the ones it
    /// deliberately does not implement; a guest that merely *references* an unimplemented
    /// call still instantiates, and only learns the truth if it actually makes the call.
    pub fn define_namespace(&mut self, module: &str, factory: impl Fn(&str) -> HostFunc + 'static) {
        self.namespaces.retain(|(m, _)| m.as_str() != module);
        self.namespaces
            .push((String::from(module), Def::Namespace(Rc::new(factory))));
    }

    /// Define a factory of last resort: any function import that matched neither an exact
    /// definition nor a namespace is backed by `factory(module, name)`.
    ///
    /// Two real uses: an embedder that wants unsatisfied imports to **trap if called**
    /// rather than fail instantiation (a module often declares more than it uses), and a
    /// host surface that is namespace-agnostic on purpose.
    ///
    /// Note what this trades away — with a fallback installed, [`Linker::resolve`] can no
    /// longer report [`LinkError::UnknownImport`] for a *function*, because nothing is
    /// unknown any more. That is the point, but it means a typo'd import name becomes a
    /// runtime surprise instead of a link-time one. Prefer explicit definitions.
    pub fn define_fallback(&mut self, factory: impl Fn(&str, &str) -> HostFunc + 'static) {
        self.fallback = Some(Rc::new(factory));
    }

    /// Is `module`.`name` resolvable — by an exact definition, a namespace catch-all, or a
    /// fallback?
    #[must_use]
    pub fn defines(&self, module: &str, name: &str) -> bool {
        self.exact(module, name).is_some()
            || self.namespace(module).is_some()
            || self.fallback.is_some()
    }

    /// Resolve `md`'s declared imports into the positional [`Imports`] the engine consumes.
    ///
    /// Walks the import list **in declaration order**, appending one backing per import, so
    /// each binds to its own slot. `store` is needed to look up the exports of instances
    /// published with [`Linker::define_instance`].
    ///
    /// # Errors
    /// [`LinkError::UnknownImport`] for a name nothing defines, [`LinkError::KindMismatch`]
    /// if the definition is the wrong kind, or [`LinkError::UnsupportedImportKind`] for an
    /// imported memory/table/tag.
    pub fn resolve(&self, store: &Store, md: &Module) -> Result<Imports, LinkError> {
        let mut imports = Imports::new();
        for imp in &md.imports {
            let kind = imp.ty.kind();
            let unknown = || LinkError::UnknownImport {
                module: imp.module.clone(),
                name: imp.name.clone(),
            };
            let mismatch = || LinkError::KindMismatch {
                module: imp.module.clone(),
                name: imp.name.clone(),
                expected: kind,
            };

            match kind {
                ExternKind::Func => {
                    imports = match self.exact(&imp.module, &imp.name) {
                        Some(Def::Func(rc)) => {
                            // Clone the `Rc`, not the closure: the definition stays in the
                            // linker so the next module can bind it too.
                            let f = Rc::clone(rc);
                            imports.with_func(move |c, a, r| f(c, a, r))
                        }
                        Some(Def::Global(_)) => return Err(mismatch()),
                        Some(Def::Instance(_) | Def::Namespace(_)) => unreachable!(
                            "instances and namespaces are stored in `namespaces`, not `defs`"
                        ),
                        None => match self.namespace(&imp.module) {
                            Some(Def::Instance(id)) => {
                                let f = store
                                    .export_func(*id, &imp.name)
                                    .ok_or_else(unknown)?;
                                imports.with_instance_func(*id, f)
                            }
                            Some(Def::Namespace(factory)) => {
                                imports.with_host_func(factory(&imp.name))
                            }
                            _ => match &self.fallback {
                                Some(factory) => {
                                    imports.with_host_func(factory(&imp.module, &imp.name))
                                }
                                None => return Err(unknown()),
                            },
                        },
                    };
                }
                ExternKind::Global => {
                    let v = match self.exact(&imp.module, &imp.name) {
                        Some(Def::Global(v)) => *v,
                        Some(Def::Func(_)) => return Err(mismatch()),
                        Some(_) => unreachable!("not stored in `defs`"),
                        None => match self.namespace(&imp.module) {
                            // A registered instance's exported global links by value.
                            Some(Def::Instance(id)) => {
                                store.export_global(*id, &imp.name).ok_or_else(unknown)?
                            }
                            _ => return Err(unknown()),
                        },
                    };
                    imports = imports.with_global(v);
                }
                // Sharing a memory or table across instances is something the *store*
                // does; there is no way to name one as a linker definition yet. Refused
                // loudly rather than skipped, so an embedder never gets a half-linked
                // instance.
                other => return Err(LinkError::UnsupportedImportKind(other)),
            }
        }
        Ok(imports)
    }

    /// Resolve `md`'s imports and instantiate it into `store`.
    ///
    /// # Errors
    /// [`LinkError`] if resolution fails. Instantiation traps are returned separately —
    /// see [`Linker::instantiate_in`] if you need to tell the two apart; this convenience
    /// form is for callers that treat any failure the same way.
    pub fn instantiate(
        &self,
        store: &mut Store,
        md: Module,
    ) -> Result<TrapResult<InstanceId>, LinkError> {
        let imports = self.resolve(store, &md)?;
        Ok(store.instantiate(md, imports))
    }

    /// As [`Linker::instantiate`], flattening the two failure modes into one error type of
    /// the caller's choosing.
    ///
    /// # Errors
    /// Whatever `on_trap` maps an instantiation trap to, or `on_link` a [`LinkError`].
    pub fn instantiate_in<E>(
        &self,
        store: &mut Store,
        md: Module,
        on_link: impl FnOnce(LinkError) -> E,
        on_trap: impl FnOnce(crate::interp::Trap) -> E,
    ) -> Result<InstanceId, E> {
        let imports = self.resolve(store, &md).map_err(on_link)?;
        store.instantiate(md, imports).map_err(on_trap)
    }

    fn insert(&mut self, module: &str, name: &str, def: Def) {
        self.defs
            .retain(|(m, n, _)| !(m.as_str() == module && n.as_str() == name));
        self.defs
            .push((String::from(module), String::from(name), def));
    }

    fn exact(&self, module: &str, name: &str) -> Option<&Def> {
        self.defs
            .iter()
            .find(|(m, n, _)| m.as_str() == module && n.as_str() == name)
            .map(|(_, _, d)| d)
    }

    fn namespace(&self, module: &str) -> Option<&Def> {
        self.namespaces
            .iter()
            .find(|(m, _)| m.as_str() == module)
            .map(|(_, d)| d)
    }
}

impl fmt::Debug for Linker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Linker")
            .field("defs", &self.defs.len())
            .field("namespaces", &self.namespaces.len())
            .finish()
    }
}

/// Convenience: wrap a closure as a [`HostFunc`] for [`Linker::define_namespace`] factories.
#[must_use]
pub fn host_func(
    f: impl Fn(&mut Caller<'_>, &[Value], &mut [Value]) -> TrapResult<()> + 'static,
) -> HostFunc {
    HostFunc::new(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{as_i32, i32_value, Trap};
    use crate::module::decode;

    fn md(src: &str) -> Module {
        decode(&crate::wat::assemble(src.as_bytes()).expect("assemble")).expect("decode")
    }

    #[test]
    fn resolves_a_named_host_function() {
        let mut l = Linker::new();
        l.define_func("env", "double", |_c, args, res| {
            res[0] = i32_value(as_i32(args[0]) * 2);
            Ok(())
        });
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module (import "env" "double" (func $d (param i32) (result i32)))
                       (func (export "go") (result i32) (call $d (i32.const 21))))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "go", &[]).unwrap()[0]), 42);
    }

    /// **The defect this module exists to prevent.** Two function imports from *different*
    /// namespaces, declared in an order that does not match the order they were defined in.
    /// A positional binding that ignored names would swap them, and both would still link —
    /// each call simply reaching the wrong host function.
    #[test]
    fn binds_by_name_not_by_definition_order() {
        let mut l = Linker::new();
        // Defined b-then-a; the module declares a-then-b.
        l.define_func("b", "f", |_c, _a, res| {
            res[0] = i32_value(200);
            Ok(())
        });
        l.define_func("a", "f", |_c, _a, res| {
            res[0] = i32_value(100);
            Ok(())
        });
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module
                       (import "a" "f" (func $a (result i32)))
                       (import "b" "f" (func $b (result i32)))
                       (func (export "a") (result i32) (call $a))
                       (func (export "b") (result i32) (call $b)))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "a", &[]).unwrap()[0]), 100);
        assert_eq!(as_i32(store.invoke(id, "b", &[]).unwrap()[0]), 200);
    }

    #[test]
    fn an_undefined_import_is_named_in_the_error() {
        let l = Linker::new();
        let mut store = Store::new();
        assert_eq!(
            l.resolve(
                &store,
                &md(r#"(module (import "env" "missing" (func)))"#)
            )
            .unwrap_err(),
            LinkError::UnknownImport {
                module: String::from("env"),
                name: String::from("missing"),
            }
        );
        let _ = &mut store;
    }

    #[test]
    fn a_definition_of_the_wrong_kind_is_a_mismatch_not_an_unknown() {
        // Diagnosing "you defined it, but as a global" is the difference between a typo and
        // a real bug, so the two must not collapse into one error.
        let mut l = Linker::new();
        l.define_global("env", "x", i32_value(1));
        let store = Store::new();
        assert_eq!(
            l.resolve(&store, &md(r#"(module (import "env" "x" (func)))"#))
                .unwrap_err(),
            LinkError::KindMismatch {
                module: String::from("env"),
                name: String::from("x"),
                expected: ExternKind::Func,
            }
        );
    }

    #[test]
    fn resolves_an_imported_global() {
        let mut l = Linker::new();
        l.define_global("env", "base", i32_value(7));
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module (import "env" "base" (global i32))
                       (func (export "go") (result i32) (global.get 0)))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "go", &[]).unwrap()[0]), 7);
    }

    #[test]
    fn a_namespace_catch_all_resolves_any_name() {
        let mut l = Linker::new();
        l.define_namespace("wasi_snapshot_preview1", |name| {
            let n = String::from(name);
            host_func(move |_c, _a, res| {
                // Answer with the length of the call's name, so the test can prove the
                // factory really saw the right one.
                res[0] = i32_value(n.len() as i32);
                Ok(())
            })
        });
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module
                       (import "wasi_snapshot_preview1" "fd_write" (func $w (result i32)))
                       (import "wasi_snapshot_preview1" "proc_exit" (func $e (result i32)))
                       (func (export "w") (result i32) (call $w))
                       (func (export "e") (result i32) (call $e)))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "w", &[]).unwrap()[0]), 8); // "fd_write"
        assert_eq!(as_i32(store.invoke(id, "e", &[]).unwrap()[0]), 9); // "proc_exit"
    }

    #[test]
    fn an_exact_definition_wins_over_the_namespace_catch_all() {
        let mut l = Linker::new();
        l.define_namespace("host", |_| {
            host_func(|_c, _a, res| {
                res[0] = i32_value(0);
                Ok(())
            })
        });
        l.define_func("host", "special", |_c, _a, res| {
            res[0] = i32_value(99);
            Ok(())
        });
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module
                       (import "host" "special" (func $s (result i32)))
                       (import "host" "other" (func $o (result i32)))
                       (func (export "s") (result i32) (call $s))
                       (func (export "o") (result i32) (call $o)))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "s", &[]).unwrap()[0]), 99);
        assert_eq!(as_i32(store.invoke(id, "o", &[]).unwrap()[0]), 0);
    }

    #[test]
    fn links_one_wasm_module_against_another() {
        let mut store = Store::new();
        let mut l = Linker::new();
        let provider = l
            .instantiate(
                &mut store,
                md(r#"(module (func (export "answer") (result i32) (i32.const 42))
                       (global (export "g") i32 (i32.const 5)))"#),
            )
            .unwrap()
            .unwrap();
        l.define_instance("lib", provider);

        let consumer = l
            .instantiate(
                &mut store,
                md(r#"(module
                       (import "lib" "answer" (func $a (result i32)))
                       (import "lib" "g" (global $g i32))
                       (func (export "go") (result i32)
                         (i32.add (call $a) (global.get $g))))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(consumer, "go", &[]).unwrap()[0]), 47);
    }

    #[test]
    fn a_linked_call_runs_against_the_exporters_own_memory() {
        // The shared-store defect class (`cmem/known-issues.md`): with every instance in one
        // store, a cross-instance call must use the CALLEE's index maps. Both modules here
        // define a memory, so the store index and the module index differ for the second —
        // exactly the case a single-instance test cannot distinguish.
        let mut store = Store::new();
        let mut l = Linker::new();
        let provider = l
            .instantiate(
                &mut store,
                md(r#"(module (memory 1) (data (i32.const 0) "\11\00\00\00")
                       (func (export "peek") (result i32) (i32.load (i32.const 0))))"#),
            )
            .unwrap()
            .unwrap();
        l.define_instance("lib", provider);

        let consumer = l
            .instantiate(
                &mut store,
                md(r#"(module
                       (import "lib" "peek" (func $p (result i32)))
                       (memory 1) (data (i32.const 0) "\99\00\00\00")
                       (func (export "go") (result i32) (call $p)))"#),
            )
            .unwrap()
            .unwrap();
        // 0x11 = the PROVIDER's memory. Reading 0x99 would mean the callee ran against the
        // caller's memory.
        assert_eq!(as_i32(store.invoke(consumer, "go", &[]).unwrap()[0]), 0x11);
    }

    #[test]
    fn an_imported_memory_is_refused_loudly() {
        let l = Linker::new();
        let store = Store::new();
        assert_eq!(
            l.resolve(&store, &md(r#"(module (import "env" "m" (memory 1)))"#))
                .unwrap_err(),
            LinkError::UnsupportedImportKind(ExternKind::Memory)
        );
    }

    #[test]
    fn redefining_a_name_replaces_it() {
        let mut l = Linker::new();
        l.define_func("env", "f", |_c, _a, res| {
            res[0] = i32_value(1);
            Ok(())
        });
        l.define_func("env", "f", |_c, _a, res| {
            res[0] = i32_value(2);
            Ok(())
        });
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module (import "env" "f" (func $f (result i32)))
                       (func (export "go") (result i32) (call $f)))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "go", &[]).unwrap()[0]), 2);
    }

    #[test]
    fn one_linker_instantiates_several_modules() {
        // Resolution borrows definitions rather than consuming them — the embedder shape:
        // define the host surface once, run many guests against it.
        let mut l = Linker::new();
        l.define_func("env", "k", |_c, _a, res| {
            res[0] = i32_value(3);
            Ok(())
        });
        let mut store = Store::new();
        let src = r#"(module (import "env" "k" (func $k (result i32)))
                      (func (export "go") (result i32) (call $k)))"#;
        for _ in 0..3 {
            let id = l.instantiate(&mut store, md(src)).unwrap().unwrap();
            assert_eq!(as_i32(store.invoke(id, "go", &[]).unwrap()[0]), 3);
        }
    }

    #[test]
    fn a_host_function_can_trap_the_guest() {
        let mut l = Linker::new();
        l.define_func("env", "boom", |_c, _a, _res| Err(Trap::HostTrap));
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module (import "env" "boom" (func $b))
                       (func (export "go") (call $b)))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(store.invoke(id, "go", &[]), Err(Trap::HostTrap));
    }

    #[test]
    fn a_host_function_reads_the_callers_memory() {
        let mut l = Linker::new();
        l.define_func("env", "sum", |caller, args, res| {
            let addr = as_i32(args[0]) as u64;
            let bytes = caller.read(0, addr, 4).ok_or(Trap::MemoryOutOfBounds)?;
            res[0] = i32_value(bytes.iter().map(|&b| i32::from(b)).sum::<i32>());
            Ok(())
        });
        let mut store = Store::new();
        let id = l
            .instantiate(
                &mut store,
                md(r#"(module
                       (import "env" "sum" (func $s (param i32) (result i32)))
                       (memory (export "memory") 1)
                       (data (i32.const 8) "\01\02\03\04")
                       (func (export "go") (result i32) (call $s (i32.const 8))))"#),
            )
            .unwrap()
            .unwrap();
        assert_eq!(as_i32(store.invoke(id, "go", &[]).unwrap()[0]), 10);
    }
}
