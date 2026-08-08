//! Rust-side tests of the C ABI.
//!
//! These call the exported `extern "C"` functions exactly as C would — through raw
//! pointers, with the same argument shapes — so they exercise the real boundary rather than
//! some Rust-friendly wrapper around it. `tests/c_smoke.c` proves the same paths link and
//! run from actual C; this file is where the *hostile* inputs live, because a null pointer
//! or a foreign handle is far easier to write here than in a C harness.

use super::*;

/// Assemble WAT to a module the C API can consume.
fn wasm(src: &str) -> Vec<u8> {
    wasmrt_core::wat::assemble(src.as_bytes()).expect("assemble")
}

/// A NUL-terminated name, kept alive by the caller.
fn cs(s: &str) -> CString {
    CString::new(s).unwrap()
}

struct Fixture {
    engine: *mut wasmrt_engine,
    store: *mut wasmrt_store,
    linker: *mut wasmrt_linker,
    module: *mut wasmrt_module,
}

impl Fixture {
    fn new(src: &str) -> Fixture {
        let engine = wasmrt_engine_new();
        let store = wasmrt_store_new(engine);
        let linker = wasmrt_linker_new(engine);
        let bytes = wasm(src);
        let mut module: *mut wasmrt_module = core::ptr::null_mut();
        let e = wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut module);
        assert!(e.is_null(), "module_new failed: {}", msg_of_err(e));
        Fixture {
            engine,
            store,
            linker,
            module,
        }
    }

    fn instantiate(&self) -> wasmrt_instance_t {
        let mut inst = wasmrt_instance_t { id: 0 };
        let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
        let e = wasmrt_linker_instantiate(
            self.linker,
            self.store,
            self.module,
            &raw mut inst,
            &raw mut trap,
        );
        assert!(e.is_null(), "instantiate failed: {}", msg_of_err(e));
        assert!(trap.is_null(), "unexpected trap: {}", msg_of_trap(trap));
        inst
    }

    fn func(&self, inst: wasmrt_instance_t, name: &str) -> wasmrt_func_t {
        let n = cs(name);
        let mut f = wasmrt_func_t { id: 0 };
        assert!(
            wasmrt_instance_get_func(self.store, inst, n.as_ptr(), &raw mut f),
            "no export named {name}"
        );
        f
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Teardown order is deliberately linker-before-store: an instance in the store
        // still holds any host callback the linker defined, and the Rc'd environment is
        // what makes that safe.
        wasmrt_linker_delete(self.linker);
        wasmrt_module_delete(self.module);
        wasmrt_store_delete(self.store);
        wasmrt_engine_delete(self.engine);
    }
}

fn msg_of_err(e: *mut wasmrt_error) -> String {
    if e.is_null() {
        return String::from("<none>");
    }
    let p = wasmrt_error_message(e);
    #[allow(unsafe_code, reason = "reading a message this crate itself produced")]
    // SAFETY: `p` came from `wasmrt_error_message` on a live error we own.
    let s = unsafe { core::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    s
}

fn msg_of_trap(t: *mut wasmrt_trap) -> String {
    if t.is_null() {
        return String::from("<none>");
    }
    let p = wasmrt_trap_message(t);
    #[allow(unsafe_code, reason = "reading a message this crate itself produced")]
    // SAFETY: `p` came from `wasmrt_trap_message` on a live trap we own.
    let s = unsafe { core::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    s
}

fn i32v(x: i32) -> wasmrt_val_t {
    wasmrt_val_t {
        kind: wasmrt_valkind_t::I32,
        of: wasmrt_val_union { i32_: x },
    }
}

fn as_i32v(v: &wasmrt_val_t) -> i32 {
    assert_eq!(v.kind, wasmrt_valkind_t::I32);
    #[allow(unsafe_code, reason = "reading the union member the tag selects")]
    // SAFETY: the tag was just asserted to be I32.
    unsafe {
        v.of.i32_
    }
}

const ADD: &str = r#"(module
    (func (export "add") (param i32 i32) (result i32)
      (i32.add (local.get 0) (local.get 1))))"#;

// ---- the happy path ------------------------------------------------------------------

#[test]
fn abi_version_matches_the_header() {
    // `wasmrt.h` hardcodes WASMRT_ABI_VERSION; if this drifts, a dynamically-bound caller
    // silently talks to the wrong ABI.
    assert_eq!(wasmrt_abi_version(), 1);
}

#[test]
fn compiles_instantiates_and_calls() {
    let f = Fixture::new(ADD);
    let inst = f.instantiate();
    let add = f.func(inst, "add");
    let args = [i32v(40), i32v(2)];
    let mut results = [i32v(0)];
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    let e = wasmrt_func_call(
        f.store,
        add,
        args.as_ptr(),
        args.len(),
        results.as_mut_ptr(),
        results.len(),
        &raw mut trap,
    );
    assert!(e.is_null(), "{}", msg_of_err(e));
    assert!(trap.is_null());
    assert_eq!(as_i32v(&results[0]), 42);
}

#[test]
fn reads_and_writes_linear_memory_both_ways() {
    let f = Fixture::new(
        r#"(module (memory (export "memory") 1)
            (data (i32.const 0) "hello"))"#,
    );
    let inst = f.instantiate();
    let n = cs("memory");
    let mut mem = wasmrt_memory_t { id: 0 };
    assert!(wasmrt_instance_get_memory(f.store, inst, n.as_ptr(), &raw mut mem));

    // The checked path.
    let mut buf = [0u8; 5];
    assert!(wasmrt_memory_read(
        f.store,
        mem,
        0,
        buf.as_mut_ptr().cast(),
        5
    ));
    assert_eq!(&buf, b"hello");

    assert!(wasmrt_memory_write(
        f.store,
        mem,
        0,
        b"world".as_ptr().cast(),
        5
    ));
    assert!(wasmrt_memory_read(
        f.store,
        mem,
        0,
        buf.as_mut_ptr().cast(),
        5
    ));
    assert_eq!(&buf, b"world");

    // The raw path must agree with it.
    assert_eq!(wasmrt_memory_data_size(f.store, mem), 65536);
    assert_eq!(wasmrt_memory_size_pages(f.store, mem), 1);
    let p = wasmrt_memory_data(f.store, mem);
    assert!(!p.is_null());
    #[allow(unsafe_code, reason = "reading through the raw view this API just returned")]
    // SAFETY: `p` points at the live memory of a store we own, unmodified since the call.
    let first = unsafe { core::slice::from_raw_parts(p, 5) };
    assert_eq!(first, b"world");
}

#[test]
fn reads_an_exported_global() {
    let f = Fixture::new(r#"(module (global (export "g") i32 (i32.const 7)))"#);
    let inst = f.instantiate();
    let n = cs("g");
    let mut g = wasmrt_global_t { id: 0 };
    assert!(wasmrt_instance_get_global(f.store, inst, n.as_ptr(), &raw mut g));
    let mut v = i32v(0);
    assert!(wasmrt_global_get(f.store, g, &raw mut v));
    assert_eq!(as_i32v(&v), 7);
}

#[test]
fn a_guest_trap_is_reported_through_trap_out_not_as_an_error() {
    // The distinction the header insists on: a trap is the guest misbehaving, an error is
    // the embedder misusing the API. Collapsing them would make both unactionable.
    let f = Fixture::new(r#"(module (func (export "boom") (unreachable)))"#);
    let inst = f.instantiate();
    let boom = f.func(inst, "boom");
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    let e = wasmrt_func_call(f.store, boom, core::ptr::null(), 0, core::ptr::null_mut(), 0, &raw mut trap);
    assert!(e.is_null(), "a guest trap must not be an error");
    assert!(!trap.is_null(), "the trap must be reported");
    assert!(
        msg_of_trap(trap).contains("unreachable"),
        "message was {:?}",
        msg_of_trap(trap)
    );
    wasmrt_trap_delete(trap);
}

#[test]
fn the_trap_frame_api_reports_the_whole_guest_stack() {
    let f = Fixture::new(
        r#"(module
             (func $bottom (unreachable))
             (func $middle (call $bottom))
             (func (export "boom") (call $middle)))"#,
    );
    let inst = f.instantiate();
    let boom = f.func(inst, "boom");
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    wasmrt_func_call(f.store, boom, core::ptr::null(), 0, core::ptr::null_mut(), 0, &raw mut trap);
    assert_eq!(wasmrt_trap_frame_count(trap), 3);

    // Innermost first, and each frame's offset must be its own — a constant would pass a
    // count-only check.
    let mut seen = Vec::new();
    for i in 0..3 {
        let (mut idx, mut off) = (u32::MAX, u32::MAX);
        assert!(wasmrt_trap_frame(trap, i, &raw mut idx, &raw mut off, core::ptr::null_mut()));
        seen.push((idx, off));
    }
    assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert!(seen[0].1 < seen[1].1 && seen[1].1 < seen[2].1, "offsets: {seen:?}");

    // Past the end is a clean false, not a stale frame.
    assert!(!wasmrt_trap_frame(
        trap,
        3,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut()
    ));
    wasmrt_trap_delete(trap);
}

#[test]
fn a_trap_outlives_the_store_state_that_produced_it() {
    let f = Fixture::new(
        r#"(module
             (func (export "boom") (unreachable))
             (func (export "fine") (result i32) (i32.const 1)))"#,
    );
    let inst = f.instantiate();
    let boom = f.func(inst, "boom");
    let fine = f.func(inst, "fine");
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    wasmrt_func_call(f.store, boom, core::ptr::null(), 0, core::ptr::null_mut(), 0, &raw mut trap);
    let (mut idx, mut off) = (u32::MAX, u32::MAX);
    assert!(wasmrt_trap_frame(trap, 0, &raw mut idx, &raw mut off, core::ptr::null_mut()));

    // The engine keeps ONE backtrace and this successful call clears it. The trap holds a copy, so
    // it must still read the same — that is what the snapshot in `engine_trap` buys.
    let mut out = [i32v(0); 1];
    let mut trap2: *mut wasmrt_trap = core::ptr::null_mut();
    wasmrt_func_call(f.store, fine, core::ptr::null(), 0, out.as_mut_ptr(), 1, &raw mut trap2);
    assert!(trap2.is_null(), "the second call must not trap");
    assert_eq!(wasmrt_trap_frame_count(trap), 1);
    let (mut idx2, mut off2) = (u32::MAX, u32::MAX);
    assert!(wasmrt_trap_frame(trap, 0, &raw mut idx2, &raw mut off2, core::ptr::null_mut()));
    assert_eq!((idx, off), (idx2, off2));
    wasmrt_trap_delete(trap);
}

/// A host-raised trap has no guest stack and must not borrow one from the engine.
#[test]
fn a_host_trap_reports_no_frames() {
    let t = wasmrt_trap_new(c"from the host".as_ptr());
    assert_eq!(wasmrt_trap_frame_count(t), 0);
    assert!(!wasmrt_trap_frame(
        t,
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut()
    ));
    wasmrt_trap_delete(t);
}

// ---- host callbacks ------------------------------------------------------------------

#[allow(unsafe_code, reason = "a C callback declaration; the ABI requires it")]
unsafe extern "C" fn triple(
    env: *mut c_void,
    _caller: *mut c_void,
    args: *const wasmrt_val_t,
    nargs: usize,
    results: *mut wasmrt_val_t,
    nresults: usize,
) -> *mut wasmrt_trap {
    assert_eq!(nargs, 1);
    assert_eq!(nresults, 1);
    #[allow(unsafe_code, reason = "the callback contract, exercised as C would")]
    // SAFETY: the engine passes arrays of exactly the declared arity, asserted above.
    unsafe {
        let x = (*args).of.i32_;
        let factor = if env.is_null() { 3 } else { *env.cast::<i32>() };
        *results = i32v(x * factor);
    }
    core::ptr::null_mut()
}

#[test]
fn a_host_import_is_called_with_its_environment() {
    let engine = wasmrt_engine_new();
    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);

    let params = [wasmrt_valkind_t::I32];
    let results = [wasmrt_valkind_t::I32];
    let ty = wasmrt_functype_new(params.as_ptr(), 1, results.as_ptr(), 1);
    let (m, n) = (cs("env"), cs("triple"));
    let mut factor: i32 = 10;
    let e = wasmrt_linker_define_func(
        linker,
        m.as_ptr(),
        n.as_ptr(),
        ty,
        Some(triple),
        (&raw mut factor).cast(),
        None,
    );
    assert!(e.is_null());
    wasmrt_functype_delete(ty);

    let bytes = wasm(
        r#"(module
            (import "env" "triple" (func $t (param i32) (result i32)))
            (func (export "go") (result i32) (call $t (i32.const 4))))"#,
    );
    let mut module: *mut wasmrt_module = core::ptr::null_mut();
    assert!(wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut module).is_null());

    let mut inst = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    assert!(
        wasmrt_linker_instantiate(linker, store, module, &raw mut inst, &raw mut trap).is_null()
    );

    let gn = cs("go");
    let mut go = wasmrt_func_t { id: 0 };
    assert!(wasmrt_instance_get_func(store, inst, gn.as_ptr(), &raw mut go));
    let mut out = [i32v(0)];
    assert!(
        wasmrt_func_call(store, go, core::ptr::null(), 0, out.as_mut_ptr(), 1, &raw mut trap)
            .is_null()
    );
    assert_eq!(as_i32v(&out[0]), 40, "the env pointer must reach the callback");

    wasmrt_linker_delete(linker);
    wasmrt_module_delete(module);
    wasmrt_store_delete(store);
    wasmrt_engine_delete(engine);
}

#[allow(unsafe_code, reason = "a C callback declaration; the ABI requires it")]
unsafe extern "C" fn always_traps(
    _env: *mut c_void,
    _caller: *mut c_void,
    _args: *const wasmrt_val_t,
    _nargs: usize,
    _results: *mut wasmrt_val_t,
    _nresults: usize,
) -> *mut wasmrt_trap {
    trap_obj("the host said no")
}

#[test]
fn a_host_callbacks_trap_message_survives_the_round_trip() {
    // `wasmrt_core::Trap` is a closed enum, so a host trap crosses back as `HostTrap` and
    // its text would be lost without the parking slot. Pin that it is not.
    let engine = wasmrt_engine_new();
    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);
    let ty = wasmrt_functype_new(core::ptr::null(), 0, core::ptr::null(), 0);
    let (m, n) = (cs("env"), cs("no"));
    wasmrt_linker_define_func(
        linker,
        m.as_ptr(),
        n.as_ptr(),
        ty,
        Some(always_traps),
        core::ptr::null_mut(),
        None,
    );
    wasmrt_functype_delete(ty);

    let bytes = wasm(
        r#"(module (import "env" "no" (func $n)) (func (export "go") (call $n)))"#,
    );
    let mut module: *mut wasmrt_module = core::ptr::null_mut();
    wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut module);
    let mut inst = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    wasmrt_linker_instantiate(linker, store, module, &raw mut inst, &raw mut trap);
    let gn = cs("go");
    let mut go = wasmrt_func_t { id: 0 };
    assert!(wasmrt_instance_get_func(store, inst, gn.as_ptr(), &raw mut go));
    wasmrt_func_call(store, go, core::ptr::null(), 0, core::ptr::null_mut(), 0, &raw mut trap);
    assert_eq!(msg_of_trap(trap), "the host said no");
    wasmrt_trap_delete(trap);

    wasmrt_linker_delete(linker);
    wasmrt_module_delete(module);
    wasmrt_store_delete(store);
    wasmrt_engine_delete(engine);
}

static mut FINALIZED: bool = false;

#[allow(unsafe_code, reason = "a C finalizer declaration; the ABI requires it")]
unsafe extern "C" fn note_finalized(_env: *mut c_void) {
    #[allow(unsafe_code, reason = "a test-local flag written from the finalizer")]
    // SAFETY: the tests in this file are single-threaded and only this one reads the flag.
    unsafe {
        FINALIZED = true;
    }
}

#[test]
fn the_env_finalizer_runs_after_the_last_holder_goes() {
    // The lifecycle hazard the Rc closes: an instance keeps the callback it linked, so
    // deleting the LINKER first must not finalize an environment the instance still points
    // at. Delete in the dangerous order and check the finalizer waits.
    #[allow(unsafe_code, reason = "resetting a test-local flag")]
    // SAFETY: single-threaded test.
    unsafe {
        FINALIZED = false;
    }

    let engine = wasmrt_engine_new();
    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);
    let ty = wasmrt_functype_new(core::ptr::null(), 0, core::ptr::null(), 0);
    let (m, n) = (cs("env"), cs("f"));
    wasmrt_linker_define_func(
        linker,
        m.as_ptr(),
        n.as_ptr(),
        ty,
        Some(always_traps),
        core::ptr::null_mut(),
        Some(note_finalized),
    );
    wasmrt_functype_delete(ty);

    let bytes = wasm(r#"(module (import "env" "f" (func $f)) (func (export "go") (call $f)))"#);
    let mut module: *mut wasmrt_module = core::ptr::null_mut();
    wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut module);
    let mut inst = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    wasmrt_linker_instantiate(linker, store, module, &raw mut inst, &raw mut trap);

    // The dangerous order.
    wasmrt_linker_delete(linker);
    #[allow(unsafe_code, reason = "reading a test-local flag")]
    // SAFETY: single-threaded test.
    let after_linker = unsafe { FINALIZED };
    assert!(
        !after_linker,
        "the instance still holds the callback; finalizing here would be a use-after-free"
    );

    wasmrt_store_delete(store);
    #[allow(unsafe_code, reason = "reading a test-local flag")]
    // SAFETY: single-threaded test.
    let after_store = unsafe { FINALIZED };
    assert!(after_store, "the finalizer must run once the last holder goes");

    wasmrt_module_delete(module);
    wasmrt_engine_delete(engine);
}

// ---- hostile input -------------------------------------------------------------------

#[test]
fn every_entry_point_survives_a_null_pointer() {
    // A C caller WILL pass NULL. None of these may crash; each must report cleanly.
    let cfg = wasmrt_config_new();
    assert!(!cfg.is_null()); // sanity: the constructor works
    wasmrt_config_delete(cfg);
    wasmrt_config_delete(core::ptr::null_mut());
    wasmrt_engine_delete(core::ptr::null_mut());
    wasmrt_store_delete(core::ptr::null_mut());
    wasmrt_module_delete(core::ptr::null_mut());
    wasmrt_linker_delete(core::ptr::null_mut());
    wasmrt_functype_delete(core::ptr::null_mut());
    wasmrt_trap_delete(core::ptr::null_mut());
    wasmrt_error_delete(core::ptr::null_mut());
    wasmrt_wasi_config_delete(core::ptr::null_mut());

    assert!(!wasmrt_config_set_feature(core::ptr::null_mut(), 0, true));
    assert!(wasmrt_store_new(core::ptr::null_mut()).is_null());
    assert!(wasmrt_linker_new(core::ptr::null_mut()).is_null());
    assert_eq!(wasmrt_module_export_count(core::ptr::null()), 0);
    assert_eq!(wasmrt_module_import_count(core::ptr::null()), 0);
    assert_eq!(wasmrt_functype_param_count(core::ptr::null()), 0);
    assert!(wasmrt_trap_message(core::ptr::null()).is_null());
    assert!(wasmrt_error_message(core::ptr::null()).is_null());
    assert_eq!(wasmrt_memory_data_size(core::ptr::null(), wasmrt_memory_t { id: 0 }), 0);

    let e = wasmrt_module_new(core::ptr::null_mut(), core::ptr::null(), 0, core::ptr::null_mut());
    assert!(!e.is_null());
    wasmrt_error_delete(e);
}

#[test]
fn a_zero_initialized_handle_is_never_valid() {
    // `wasmrt_func_t f = {0};` is what a C programmer writes by default. It must not name
    // slot 0 — hence the +1 in the handle packing.
    let f = Fixture::new(ADD);
    let _inst = f.instantiate();
    assert!(!wasmrt_instance_is_valid(f.store, wasmrt_instance_t { id: 0 }));
    assert!(!wasmrt_func_is_valid(f.store, wasmrt_func_t { id: 0 }));
    assert!(!wasmrt_memory_is_valid(f.store, wasmrt_memory_t { id: 0 }));
    assert!(!wasmrt_global_is_valid(f.store, wasmrt_global_t { id: 0 }));
}

#[test]
fn a_handle_from_another_store_is_rejected_not_aliased() {
    // THE defect this design exists to prevent. Two stores, each with one instance, so the
    // slot numbers are IDENTICAL — only the store tag distinguishes them. Without it, `b`'s
    // handle would index straight into `a`'s resources and quietly call the wrong function.
    let a = Fixture::new(ADD);
    let b = Fixture::new(r#"(module (func (export "add") (param i32 i32) (result i32)
                              (i32.const 999)))"#);
    let ia = a.instantiate();
    let ib = b.instantiate();
    let fa = a.func(ia, "add");
    let fb = b.func(ib, "add");

    // The raw slots really do collide — otherwise this test would pass for the wrong reason.
    assert_eq!(fa.id & 0xffff_ffff, fb.id & 0xffff_ffff);

    assert!(wasmrt_func_is_valid(a.store, fa));
    assert!(!wasmrt_func_is_valid(a.store, fb), "b's handle must not be valid in a");
    assert!(!wasmrt_instance_is_valid(a.store, ib));

    let args = [i32v(1), i32v(1)];
    let mut out = [i32v(0)];
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    let e = wasmrt_func_call(a.store, fb, args.as_ptr(), 2, out.as_mut_ptr(), 1, &raw mut trap);
    assert!(!e.is_null(), "calling a foreign handle must be an error");
    assert!(msg_of_err(e).contains("does not belong to this store"));
    wasmrt_error_delete(e);
}

#[test]
fn arity_mismatches_are_errors_not_silent_truncation() {
    let f = Fixture::new(ADD);
    let inst = f.instantiate();
    let add = f.func(inst, "add");
    let args = [i32v(1)];
    let mut out = [i32v(0)];
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();

    let e = wasmrt_func_call(f.store, add, args.as_ptr(), 1, out.as_mut_ptr(), 1, &raw mut trap);
    assert!(!e.is_null());
    assert!(msg_of_err(e).contains("expected 2 argument"));
    wasmrt_error_delete(e);

    let args2 = [i32v(1), i32v(2)];
    let e = wasmrt_func_call(f.store, add, args2.as_ptr(), 2, out.as_mut_ptr(), 0, &raw mut trap);
    assert!(!e.is_null());
    assert!(msg_of_err(e).contains("expected room for 1 result"));
    wasmrt_error_delete(e);
}

#[test]
fn an_out_of_bounds_memory_access_is_refused_and_copies_nothing() {
    let f = Fixture::new(r#"(module (memory (export "memory") 1))"#);
    let inst = f.instantiate();
    let n = cs("memory");
    let mut mem = wasmrt_memory_t { id: 0 };
    assert!(wasmrt_instance_get_memory(f.store, inst, n.as_ptr(), &raw mut mem));

    let mut buf = [0xAAu8; 4];
    assert!(!wasmrt_memory_read(f.store, mem, 65534, buf.as_mut_ptr().cast(), 4));
    assert_eq!(buf, [0xAA; 4], "a refused read must not have copied");

    // The overflow case: offset + n wraps if it is not checked first.
    assert!(!wasmrt_memory_read(f.store, mem, u64::MAX, buf.as_mut_ptr().cast(), 4));
    assert!(!wasmrt_memory_write(f.store, mem, u64::MAX, buf.as_ptr().cast(), 4));
}

// ---- configuration -------------------------------------------------------------------

#[test]
fn a_disabled_proposal_makes_the_module_invalid() {
    let cfg = wasmrt_config_new();
    assert!(wasmrt_config_set_feature(cfg, 6 /* SIMD */, false));
    assert!(wasmrt_config_set_feature(cfg, 7 /* relaxed SIMD */, false));
    let mut e: *mut wasmrt_error = core::ptr::null_mut();
    let engine = wasmrt_engine_new_with_config(cfg, &raw mut e);
    assert!(!engine.is_null(), "{}", msg_of_err(e));
    wasmrt_config_delete(cfg);

    let bytes = wasm(r#"(module (func (result v128) (v128.const i32x4 0 0 0 0)))"#);
    assert!(!wasmrt_module_validate(engine, bytes.as_ptr(), bytes.len()));
    let mut m: *mut wasmrt_module = core::ptr::null_mut();
    let err = wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut m);
    assert!(!err.is_null());
    assert!(msg_of_err(err).contains("simd"), "{}", msg_of_err(err));
    wasmrt_error_delete(err);

    // A plain module still works on the same engine.
    let ok = wasm(ADD);
    assert!(wasmrt_module_validate(engine, ok.as_ptr(), ok.len()));
    wasmrt_engine_delete(engine);
}

#[test]
fn an_incoherent_config_is_reported_not_repaired() {
    let cfg = wasmrt_config_new();
    assert!(wasmrt_config_set_feature(cfg, 11 /* function-references */, false));
    // GC still on -> incoherent.
    let mut e: *mut wasmrt_error = core::ptr::null_mut();
    let engine = wasmrt_engine_new_with_config(cfg, &raw mut e);
    assert!(engine.is_null(), "an incoherent config must not produce an engine");
    assert!(!e.is_null());
    assert!(msg_of_err(e).contains("requires"), "{}", msg_of_err(e));
    wasmrt_error_delete(e);
    wasmrt_config_delete(cfg);
}

#[test]
fn an_unknown_feature_index_is_rejected() {
    let cfg = wasmrt_config_new();
    assert!(!wasmrt_config_set_feature(cfg, 999, false));
    let mut got = true;
    assert!(!wasmrt_config_get_feature(cfg, 999, &raw mut got));
    assert!(wasmrt_config_get_feature(cfg, 6, &raw mut got));
    assert!(got, "SIMD is on by default");
    wasmrt_config_delete(cfg);
}

#[test]
fn a_lowered_memory_ceiling_reaches_the_engine() {
    let cfg = wasmrt_config_new();
    wasmrt_config_set_max_memory_bytes(cfg, 65536); // one page
    let mut e: *mut wasmrt_error = core::ptr::null_mut();
    let engine = wasmrt_engine_new_with_config(cfg, &raw mut e);
    assert!(!engine.is_null());
    wasmrt_config_delete(cfg);

    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);
    let bytes = wasm(r#"(module (memory 4))"#);
    let mut m: *mut wasmrt_module = core::ptr::null_mut();
    assert!(wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut m).is_null());
    let mut inst = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    wasmrt_linker_instantiate(linker, store, m, &raw mut inst, &raw mut trap);
    assert!(!trap.is_null(), "4 pages must not fit under a 1-page ceiling");
    wasmrt_trap_delete(trap);

    wasmrt_linker_delete(linker);
    wasmrt_module_delete(m);
    wasmrt_store_delete(store);
    wasmrt_engine_delete(engine);
}

// ---- linking -------------------------------------------------------------------------

#[test]
fn an_unresolved_import_names_itself_in_the_error() {
    let engine = wasmrt_engine_new();
    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);
    let bytes = wasm(r#"(module (import "env" "nope" (func)))"#);
    let mut m: *mut wasmrt_module = core::ptr::null_mut();
    wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut m);
    let mut inst = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    let e = wasmrt_linker_instantiate(linker, store, m, &raw mut inst, &raw mut trap);
    assert!(!e.is_null());
    let text = msg_of_err(e);
    assert!(text.contains("env") && text.contains("nope"), "{text}");
    wasmrt_error_delete(e);

    wasmrt_linker_delete(linker);
    wasmrt_module_delete(m);
    wasmrt_store_delete(store);
    wasmrt_engine_delete(engine);
}

#[test]
fn unknown_imports_as_traps_defers_the_failure_to_the_call() {
    let engine = wasmrt_engine_new();
    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);
    assert!(wasmrt_linker_define_unknown_imports_as_traps(linker).is_null());

    // Declares an import it never calls: instantiation must succeed.
    let bytes = wasm(
        r#"(module (import "env" "unused" (func $u))
            (func (export "fine") (result i32) (i32.const 5))
            (func (export "bad") (call $u)))"#,
    );
    let mut m: *mut wasmrt_module = core::ptr::null_mut();
    wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut m);
    let mut inst = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    assert!(wasmrt_linker_instantiate(linker, store, m, &raw mut inst, &raw mut trap).is_null());

    let fine = cs("fine");
    let mut fh = wasmrt_func_t { id: 0 };
    assert!(wasmrt_instance_get_func(store, inst, fine.as_ptr(), &raw mut fh));
    let mut out = [i32v(0)];
    assert!(wasmrt_func_call(store, fh, core::ptr::null(), 0, out.as_mut_ptr(), 1, &raw mut trap).is_null());
    assert_eq!(as_i32v(&out[0]), 5);

    // Calling the stub traps, naming what was missing.
    let bad = cs("bad");
    let mut bh = wasmrt_func_t { id: 0 };
    assert!(wasmrt_instance_get_func(store, inst, bad.as_ptr(), &raw mut bh));
    wasmrt_func_call(store, bh, core::ptr::null(), 0, core::ptr::null_mut(), 0, &raw mut trap);
    assert!(!trap.is_null());
    let t = msg_of_trap(trap);
    assert!(t.contains("unused"), "{t}");
    wasmrt_trap_delete(trap);

    wasmrt_linker_delete(linker);
    wasmrt_module_delete(m);
    wasmrt_store_delete(store);
    wasmrt_engine_delete(engine);
}

#[test]
fn one_module_links_against_another_in_the_same_store() {
    let engine = wasmrt_engine_new();
    let store = wasmrt_store_new(engine);
    let linker = wasmrt_linker_new(engine);

    let lib = wasm(r#"(module (func (export "answer") (result i32) (i32.const 42)))"#);
    let mut lm: *mut wasmrt_module = core::ptr::null_mut();
    wasmrt_module_new(engine, lib.as_ptr(), lib.len(), &raw mut lm);
    let mut li = wasmrt_instance_t { id: 0 };
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    assert!(wasmrt_linker_instantiate(linker, store, lm, &raw mut li, &raw mut trap).is_null());

    let ns = cs("lib");
    assert!(wasmrt_linker_define_instance(linker, store, ns.as_ptr(), li).is_null());

    let app = wasm(
        r#"(module (import "lib" "answer" (func $a (result i32)))
            (func (export "go") (result i32) (call $a)))"#,
    );
    let mut am: *mut wasmrt_module = core::ptr::null_mut();
    wasmrt_module_new(engine, app.as_ptr(), app.len(), &raw mut am);
    let mut ai = wasmrt_instance_t { id: 0 };
    assert!(wasmrt_linker_instantiate(linker, store, am, &raw mut ai, &raw mut trap).is_null());

    let gn = cs("go");
    let mut go = wasmrt_func_t { id: 0 };
    assert!(wasmrt_instance_get_func(store, ai, gn.as_ptr(), &raw mut go));
    let mut out = [i32v(0)];
    assert!(wasmrt_func_call(store, go, core::ptr::null(), 0, out.as_mut_ptr(), 1, &raw mut trap).is_null());
    assert_eq!(as_i32v(&out[0]), 42);

    wasmrt_linker_delete(linker);
    wasmrt_module_delete(lm);
    wasmrt_module_delete(am);
    wasmrt_store_delete(store);
    wasmrt_engine_delete(engine);
}

// ---- introspection -------------------------------------------------------------------

#[test]
fn enumerates_exports_and_imports() {
    let f = Fixture::new(
        r#"(module
            (import "a" "x" (func))
            (import "b" "y" (global i32))
            (memory (export "memory") 1)
            (func (export "run")))"#,
    );
    assert_eq!(wasmrt_module_export_count(f.module), 2);
    assert_eq!(wasmrt_module_import_count(f.module), 2);

    let mut name: *const c_char = core::ptr::null();
    let mut len = 0usize;
    let mut kind = wasmrt_externkind_t::Func;
    assert!(wasmrt_module_export(f.module, 0, &raw mut name, &raw mut len, &raw mut kind));
    assert_eq!(len, 6);
    assert_eq!(kind, wasmrt_externkind_t::Memory);
    assert!(!wasmrt_module_export(f.module, 99, &raw mut name, &raw mut len, &raw mut kind));

    let mut mname: *const c_char = core::ptr::null();
    let mut mlen = 0usize;
    assert!(wasmrt_module_import(
        f.module,
        1,
        &raw mut mname,
        &raw mut mlen,
        &raw mut name,
        &raw mut len,
        &raw mut kind
    ));
    assert_eq!(kind, wasmrt_externkind_t::Global);
}

#[test]
fn reports_a_functions_signature() {
    let f = Fixture::new(ADD);
    let inst = f.instantiate();
    let add = f.func(inst, "add");
    let ty = wasmrt_func_type(f.store, add);
    assert!(!ty.is_null());
    assert_eq!(wasmrt_functype_param_count(ty), 2);
    assert_eq!(wasmrt_functype_result_count(ty), 1);
    let mut k = wasmrt_valkind_t::I64;
    assert!(wasmrt_functype_param(ty, 0, &raw mut k));
    assert_eq!(k, wasmrt_valkind_t::I32);
    assert!(!wasmrt_functype_param(ty, 7, &raw mut k));
    wasmrt_functype_delete(ty);
}

#[test]
fn a_v128_signature_cannot_cross_the_boundary_and_says_so() {
    // The engine runs SIMD fine; it just cannot be marshalled. The refusal must be an
    // error, not a wrong value.
    let f = Fixture::new(
        r#"(module (func (export "v") (result v128) (v128.const i32x4 1 2 3 4)))"#,
    );
    let inst = f.instantiate();
    let v = f.func(inst, "v");
    assert!(wasmrt_func_type(f.store, v).is_null());
    let mut out = [i32v(0)];
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    let e = wasmrt_func_call(f.store, v, core::ptr::null(), 0, out.as_mut_ptr(), 1, &raw mut trap);
    assert!(!e.is_null());
    assert!(msg_of_err(e).contains("cannot carry"), "{}", msg_of_err(e));
    wasmrt_error_delete(e);
}

// ---- lifecycle fuzz ------------------------------------------------------------------

/// A deterministic pseudo-random source. Seeded and reproducible on purpose: a fuzz that
/// finds a fault on Tuesday and cannot reproduce it on Wednesday has found nothing.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        // Numerical Recipes' constants; quality is irrelevant here, reproducibility is not.
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// **The gate wazmrt needed a fuzz for.** Its C ABI is a refcounted object model whose six
/// memory-safety invariants no type system checks; ours replaces that with checked handles,
/// and this is what shows the replacement holds.
///
/// Drives objects through randomized creation, use and destruction orders — including the
/// orders an embedder is *not* supposed to use — and touches handles after their store is
/// gone. Under **Miri** (`scripts/miri-gate.sh`) a use-after-free or double-free here is a
/// hard error rather than a value that happens to look plausible; run plain, it still
/// checks that none of it panics or reports nonsense.
#[test]
fn lifecycle_fuzz() {
    let mut rng = Lcg(0x5EED_1234_ABCD_0001);
    // Miri interprets every instruction, so the same loop count would take minutes there.
    let rounds = if cfg!(miri) { 12 } else { 400 };

    for _round in 0..rounds {
        let engine = wasmrt_engine_new();
        let store = wasmrt_store_new(engine);
        let linker = wasmrt_linker_new(engine);

        // Sometimes register a host function with a finalizer, so the Rc'd environment is
        // exercised across every teardown order below.
        // 3, so `triple(2)` gives 6 — the same answer the import-free variant computes
        // directly, letting one assertion cover both shapes.
        let boxed_env: Box<i32> = Box::new(3);
        let env_ptr = Box::into_raw(boxed_env);
        let use_host = rng.below(2) == 0;
        let (m, n) = (cs("env"), cs("triple"));
        if use_host {
            let params = [wasmrt_valkind_t::I32];
            let results = [wasmrt_valkind_t::I32];
            let ty = wasmrt_functype_new(params.as_ptr(), 1, results.as_ptr(), 1);
            wasmrt_linker_define_func(
                linker,
                m.as_ptr(),
                n.as_ptr(),
                ty,
                Some(triple),
                env_ptr.cast(),
                None, // freed by hand below; the finalizer path has its own test
            );
            wasmrt_functype_delete(ty);
        }
        wasmrt_linker_define_unknown_imports_as_traps(linker);

        // A few instances in one store, so handle slots collide across stores between
        // rounds — which is exactly what the store tag has to survive.
        let src = if use_host {
            r#"(module
                (import "env" "triple" (func $t (param i32) (result i32)))
                (memory (export "memory") 1)
                (global (export "g") i32 (i32.const 3))
                (func (export "go") (result i32) (call $t (i32.const 2))))"#
        } else {
            r#"(module
                (memory (export "memory") 1)
                (global (export "g") i32 (i32.const 3))
                (func (export "go") (result i32) (i32.const 6)))"#
        };
        let bytes = wasm(src);

        let mut modules: Vec<*mut wasmrt_module> = Vec::new();
        let mut instances: Vec<wasmrt_instance_t> = Vec::new();
        let count = 1 + rng.below(3);
        for _ in 0..count {
            let mut md: *mut wasmrt_module = core::ptr::null_mut();
            let e = wasmrt_module_new(engine, bytes.as_ptr(), bytes.len(), &raw mut md);
            assert!(e.is_null(), "{}", msg_of_err(e));
            let mut inst = wasmrt_instance_t { id: 0 };
            let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
            let e = wasmrt_linker_instantiate(linker, store, md, &raw mut inst, &raw mut trap);
            assert!(e.is_null(), "{}", msg_of_err(e));
            assert!(trap.is_null());
            modules.push(md);
            instances.push(inst);
        }

        // Random reads and calls against random handles.
        let go = cs("go");
        let memn = cs("memory");
        let gn = cs("g");
        let mut mems: Vec<wasmrt_memory_t> = Vec::new();
        for _ in 0..(4 + rng.below(8)) {
            let inst = instances[rng.below(instances.len() as u64) as usize];
            match rng.below(4) {
                0 => {
                    let mut f = wasmrt_func_t { id: 0 };
                    assert!(wasmrt_instance_get_func(store, inst, go.as_ptr(), &raw mut f));
                    let mut out = [i32v(0)];
                    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
                    let e = wasmrt_func_call(
                        store, f, core::ptr::null(), 0, out.as_mut_ptr(), 1, &raw mut trap,
                    );
                    assert!(e.is_null(), "{}", msg_of_err(e));
                    assert!(trap.is_null());
                    assert_eq!(as_i32v(&out[0]), 6);
                }
                1 => {
                    let mut mem = wasmrt_memory_t { id: 0 };
                    assert!(wasmrt_instance_get_memory(store, inst, memn.as_ptr(), &raw mut mem));
                    let mut buf = [0u8; 8];
                    assert!(wasmrt_memory_write(store, mem, 16, buf.as_ptr().cast(), 8));
                    assert!(wasmrt_memory_read(store, mem, 16, buf.as_mut_ptr().cast(), 8));
                    mems.push(mem);
                }
                2 => {
                    let mut g = wasmrt_global_t { id: 0 };
                    assert!(wasmrt_instance_get_global(store, inst, gn.as_ptr(), &raw mut g));
                    let mut v = i32v(0);
                    assert!(wasmrt_global_get(store, g, &raw mut v));
                    assert_eq!(as_i32v(&v), 3);
                }
                _ => {
                    // The raw view, re-fetched each time as the header requires.
                    if let Some(&mem) = mems.first() {
                        let p = wasmrt_memory_data(store, mem);
                        assert!(!p.is_null());
                        assert!(wasmrt_memory_data_size(store, mem) >= 65536);
                    }
                }
            }
        }

        // Keep a handle past its store's death and prove it is refused rather than followed.
        let doomed = mems.first().copied();
        let doomed_inst = instances[0];

        // Randomize the teardown order, INCLUDING the orders the docs discourage.
        let order = rng.below(4);
        let drop_store = |s: *mut wasmrt_store| wasmrt_store_delete(s);
        match order {
            0 => {
                wasmrt_linker_delete(linker);
                drop_store(store);
            }
            1 => {
                drop_store(store);
                wasmrt_linker_delete(linker);
            }
            2 => {
                for &md in &modules {
                    wasmrt_module_delete(md);
                }
                modules.clear();
                wasmrt_linker_delete(linker);
                drop_store(store);
            }
            _ => {
                drop_store(store);
                for &md in &modules {
                    wasmrt_module_delete(md);
                }
                modules.clear();
                wasmrt_linker_delete(linker);
            }
        }
        for &md in &modules {
            wasmrt_module_delete(md);
        }

        // A fresh store must not honour the dead store's handles: tags are never reissued.
        let store2 = wasmrt_store_new(engine);
        if let Some(mem) = doomed {
            assert!(
                !wasmrt_memory_is_valid(store2, mem),
                "a handle from a deleted store must not validate against a new one"
            );
            assert_eq!(wasmrt_memory_data_size(store2, mem), 0);
            assert!(wasmrt_memory_data(store2, mem).is_null());
        }
        assert!(!wasmrt_instance_is_valid(store2, doomed_inst));
        wasmrt_store_delete(store2);

        wasmrt_engine_delete(engine);

        #[allow(unsafe_code, reason = "reclaiming the env box this round allocated")]
        // SAFETY: `env_ptr` came from `Box::into_raw` at the top of this iteration and has
        // not been reclaimed; no finalizer was registered, so nothing else freed it.
        unsafe {
            drop(Box::from_raw(env_ptr));
        }
    }
}

#[test]
fn a_reactor_without_initialize_is_not_an_error() {
    let f = Fixture::new(ADD);
    let inst = f.instantiate();
    let mut trap: *mut wasmrt_trap = core::ptr::null_mut();
    assert!(wasmrt_instance_initialize(f.store, inst, &raw mut trap).is_null());
    assert!(trap.is_null());
}
