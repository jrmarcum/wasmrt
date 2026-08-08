//! `wasmrt-capi` — the public **`wasmrt.h`** C ABI over `wasmrt-core`.
//!
//! **Designed, not transliterated** (T8, `cmem/roadmap.md`): a lean `wasmrt_*` surface,
//! wasmtime-*shaped* but under our own names, built on lightweight checked handles rather
//! than the wasm-c-api refcounted object model — which is `wazmrt`'s single highest-risk
//! file, six memory-safety invariants and a lifecycle fuzz to hold them up.
//!
//! # Safety posture
//!
//! `wasmrt-core` is `#![forbid(unsafe_code)]`. This crate cannot be, because a C ABI *is* an
//! unsafe boundary. It is `deny` instead, and every exception is written down at its site.
//!
//! Rather than scatter `unsafe` across sixty exported functions, **all raw-pointer work is
//! confined to [`ffi`]**, whose primitives are justified once and reject null everywhere.
//! The exported functions below are ordinary safe Rust that calls them; the only `unsafe`
//! each carries is the `#[unsafe(no_mangle)]` attribute (an assertion about symbol naming,
//! not about memory) and, where unavoidable, a single call into `ffi`.
//!
//! # The two lifecycle hazards, closed by construction
//!
//! 1. **A stale or foreign value handle.** `wasmrt_func_t` and friends are integers, and an
//!    integer from another store would otherwise index into *this* store's resources. Every
//!    handle carries the identity of the store that issued it and is checked on use, so the
//!    answer is an error, never someone else's memory.
//! 2. **A host callback outliving its linker.** An instance keeps the callback it linked
//!    against, so deleting the linker first would run the caller's `env_finalizer` while a
//!    live instance still points at `env`. The environment is therefore held behind an
//!    [`Rc`], shared by the linker *and* every closure that linked it — the finalizer runs
//!    when the last of them goes, in whatever order the embedder deletes things.

#![deny(unsafe_code)]
// The types below ARE the C names. `wasmrt_store_t` in the header and `wasmrt_store` here
// must be the same identifier for `cbindgen`-style tooling and for anyone reading the two
// side by side, so Rust's casing convention gives way to matching the ABI it describes.
#![allow(non_camel_case_types)]
// Clippy asks that a public function dereferencing a raw pointer be declared `unsafe fn`.
// That is right for a Rust API and wrong for this one: these symbols exist to be called
// from C, which has no notion of an `unsafe fn`, and marking them would only stop Rust
// callers from using the very boundary the crate exists to expose. The obligation clippy is
// pointing at is real and is instead discharged where it can be — in `ffi`, which rejects
// null everywhere and never invents a length, and in the contract `wasmrt.h` states.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

#[allow(unsafe_code, reason = "the crate's single raw-pointer boundary; see the module docs")]
pub mod ffi;

use core::cell::RefCell;
use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicU32, Ordering};
use std::ffi::CString;
use std::rc::Rc;

use wasmrt_core::features::{Feature, Features};
use wasmrt_core::interp::{
    self, Caller, InstanceId, ResourceLimits, Store, Trap, Value,
};
use wasmrt_core::linker::{host_func, LinkError, Linker};
use wasmrt_core::module::Module;
use wasmrt_core::types::{ExternKind, ValType};

// =====================================================================================
// Handles
// =====================================================================================

/// Issued to each store so its value handles cannot be confused with another's. Starts at
/// 1, so a zero-initialized handle is never valid.
static NEXT_STORE_TAG: AtomicU32 = AtomicU32::new(1);

/// Pack a store tag and a slot into a value handle.
///
/// The `+ 1` matters: it keeps `id == 0` permanently invalid, so the zero-initialized
/// `wasmrt_func_t f = {0};` a C programmer naturally writes is rejected rather than
/// silently naming slot 0.
fn pack(tag: u32, slot: usize) -> u64 {
    (u64::from(tag) << 32) | (slot as u64 + 1)
}

/// Recover the slot from a handle, or `None` if it belongs to a different store (or to a
/// store that no longer exists, whose tag is never reissued).
fn unpack(id: u64, tag: u32) -> Option<usize> {
    if (id >> 32) as u32 != tag {
        return None;
    }
    usize::try_from(id & 0xffff_ffff).ok()?.checked_sub(1)
}

macro_rules! value_handle {
    ($name:ident) => {
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct $name {
            pub id: u64,
        }
    };
}

value_handle!(wasmrt_instance_t);
value_handle!(wasmrt_func_t);
value_handle!(wasmrt_memory_t);
value_handle!(wasmrt_global_t);

// =====================================================================================
// Values
// =====================================================================================

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum wasmrt_valkind_t {
    I32 = 0,
    I64 = 1,
    F32 = 2,
    F64 = 3,
    Funcref = 4,
    Externref = 5,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union wasmrt_val_union {
    pub i32_: i32,
    pub i64_: i64,
    pub f32_: f32,
    pub f64_: f64,
    pub ref_: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct wasmrt_val_t {
    pub kind: wasmrt_valkind_t,
    pub of: wasmrt_val_union,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum wasmrt_externkind_t {
    Func = 0,
    Table = 1,
    Memory = 2,
    Global = 3,
    Tag = 4,
}

impl From<ExternKind> for wasmrt_externkind_t {
    fn from(k: ExternKind) -> Self {
        match k {
            ExternKind::Func => wasmrt_externkind_t::Func,
            ExternKind::Table => wasmrt_externkind_t::Table,
            ExternKind::Memory => wasmrt_externkind_t::Memory,
            ExternKind::Global => wasmrt_externkind_t::Global,
            ExternKind::Tag => wasmrt_externkind_t::Tag,
        }
    }
}

/// The boundary kind of a wasm value type, or `None` for one that cannot cross.
///
/// `v128` and the GC reference types are deliberately absent: a guest may use them
/// internally and run correctly, but marshalling them to C would need a representation the
/// loaders have no use for. A call that would require one is refused with an error rather
/// than given a plausible-looking wrong value.
fn kind_of(t: ValType) -> Option<wasmrt_valkind_t> {
    Some(match t {
        ValType::I32 => wasmrt_valkind_t::I32,
        ValType::I64 => wasmrt_valkind_t::I64,
        ValType::F32 => wasmrt_valkind_t::F32,
        ValType::F64 => wasmrt_valkind_t::F64,
        _ => {
            if !t.is_ref() || t == ValType::V128 {
                return None;
            }
            match t.ref_heap() {
                wasmrt_core::types::RefHeap::Func => wasmrt_valkind_t::Funcref,
                wasmrt_core::types::RefHeap::Extern => wasmrt_valkind_t::Externref,
                _ => return None, // the GC hierarchy stays inside the guest
            }
        }
    })
}

fn to_value(v: &wasmrt_val_t) -> Value {
    // Reading the union member the tag selects is the whole point of a tagged union; the
    // tag is set by the caller, and every arm below reads a member of the declared size.
    #[allow(unsafe_code, reason = "reading the union member the caller's tag selects")]
    // SAFETY: `wasmrt_val_union` is a union of Copy scalars with no invalid bit patterns
    // for the arm each tag selects, so every read below is well-defined whatever the
    // caller stored. A mismatched tag yields a garbage *value*, never unsoundness.
    unsafe {
        match v.kind {
            wasmrt_valkind_t::I32 => interp::i32_value(v.of.i32_),
            wasmrt_valkind_t::I64 => interp::i64_value(v.of.i64_),
            wasmrt_valkind_t::F32 => interp::f32_value(v.of.f32_),
            wasmrt_valkind_t::F64 => interp::f64_value(v.of.f64_),
            wasmrt_valkind_t::Funcref | wasmrt_valkind_t::Externref => Value::from(v.of.ref_),
        }
    }
}

fn from_value(kind: wasmrt_valkind_t, v: Value) -> wasmrt_val_t {
    let of = match kind {
        wasmrt_valkind_t::I32 => wasmrt_val_union {
            i32_: interp::as_i32(v),
        },
        wasmrt_valkind_t::I64 => wasmrt_val_union {
            i64_: interp::as_i64(v),
        },
        wasmrt_valkind_t::F32 => wasmrt_val_union {
            f32_: interp::as_f32(v),
        },
        wasmrt_valkind_t::F64 => wasmrt_val_union {
            f64_: interp::as_f64(v),
        },
        wasmrt_valkind_t::Funcref | wasmrt_valkind_t::Externref => wasmrt_val_union {
            ref_: v as u64,
        },
    };
    wasmrt_val_t { kind, of }
}

// =====================================================================================
// Errors and traps
// =====================================================================================

pub struct wasmrt_error {
    message: CString,
}

pub struct wasmrt_trap {
    message: CString,
}

fn err(msg: impl AsRef<str>) -> *mut wasmrt_error {
    ffi::into_raw(wasmrt_error {
        message: cstring(msg),
    })
}

fn trap_obj(msg: impl AsRef<str>) -> *mut wasmrt_trap {
    ffi::into_raw(wasmrt_trap {
        message: cstring(msg),
    })
}

/// Build a `CString`, replacing interior NULs rather than failing — a diagnostic must never
/// be lost because of the character it happened to contain.
fn cstring(s: impl AsRef<str>) -> CString {
    let cleaned: String = s
        .as_ref()
        .chars()
        .map(|c| if c == '\0' { ' ' } else { c })
        .collect();
    CString::new(cleaned).unwrap_or_else(|_| CString::new("<unrepresentable>").unwrap())
}

thread_local! {
    /// The message from the most recent host-callback trap.
    ///
    /// `wasmrt_core::Trap` is a closed enum, so a host trap crosses back into the engine as
    /// `Trap::HostTrap` and its text would otherwise be lost. The callback wrapper parks it
    /// here and the call site picks it up. Thread-local because `wasmrt.h` states a store is
    /// single-threaded, so there is exactly one call in flight per thread.
    static HOST_TRAP: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Render a trap, preferring a host callback's own message when it produced one.
fn trap_message(t: &Trap) -> CString {
    if matches!(t, Trap::HostTrap) {
        if let Some(m) = HOST_TRAP.with_borrow_mut(Option::take) {
            return m;
        }
    }
    cstring(format!("{t}"))
}

fn link_error_text(e: &LinkError) -> String {
    format!("{e}")
}

// =====================================================================================
// Config / engine
// =====================================================================================

pub struct wasmrt_config {
    features: Features,
    limits: ResourceLimits,
}

pub struct wasmrt_engine {
    features: Features,
    limits: ResourceLimits,
}

fn feature_of(i: u32) -> Option<Feature> {
    Some(match i {
        0 => Feature::SignExtension,
        1 => Feature::SaturatingFloatToInt,
        2 => Feature::MultiValue,
        3 => Feature::ReferenceTypes,
        4 => Feature::BulkMemory,
        5 => Feature::ExtendedConst,
        6 => Feature::Simd,
        7 => Feature::RelaxedSimd,
        8 => Feature::Threads,
        9 => Feature::MultiMemory,
        10 => Feature::Memory64,
        11 => Feature::FunctionReferences,
        12 => Feature::Gc,
        13 => Feature::Exceptions,
        _ => return None,
    })
}

// =====================================================================================
// Store
// =====================================================================================

pub struct wasmrt_store {
    tag: u32,
    inner: Store,
    /// Instances in creation order; a `wasmrt_instance_t` indexes this.
    instances: Vec<InstanceId>,
    /// Records behind the func/memory/global handles: which instance, and which index in
    /// that instance's own index space.
    funcs: Vec<(usize, u32)>,
    memories: Vec<(usize, u32)>,
    globals: Vec<(usize, u32)>,
}

impl wasmrt_store {
    fn instance(&self, h: wasmrt_instance_t) -> Option<InstanceId> {
        self.instances.get(unpack(h.id, self.tag)?).copied()
    }
    fn func(&self, h: wasmrt_func_t) -> Option<(InstanceId, u32)> {
        let (i, f) = *self.funcs.get(unpack(h.id, self.tag)?)?;
        Some((*self.instances.get(i)?, f))
    }
    fn memory(&self, h: wasmrt_memory_t) -> Option<(InstanceId, u32)> {
        let (i, m) = *self.memories.get(unpack(h.id, self.tag)?)?;
        Some((*self.instances.get(i)?, m))
    }
    fn global(&self, h: wasmrt_global_t) -> Option<(InstanceId, u32)> {
        let (i, g) = *self.globals.get(unpack(h.id, self.tag)?)?;
        Some((*self.instances.get(i)?, g))
    }
    /// The position of an `InstanceId` in `instances`, for building dependent handles.
    fn slot_of(&self, id: InstanceId) -> Option<usize> {
        self.instances.iter().position(|&x| x == id)
    }
}

// =====================================================================================
// Module / functype
// =====================================================================================

pub struct wasmrt_module {
    md: Module,
    /// NUL-terminated copies of every export and import name, so the borrowed `const char *`
    /// handed to C is a real C string for the module's lifetime. The decoded `Module` holds
    /// Rust `String`s, which are not NUL-terminated.
    export_names: Vec<CString>,
    import_modules: Vec<CString>,
    import_names: Vec<CString>,
}

pub struct wasmrt_functype {
    params: Vec<wasmrt_valkind_t>,
    results: Vec<wasmrt_valkind_t>,
}

// =====================================================================================
// Linker
// =====================================================================================

/// A host callback's environment pointer plus its finalizer.
///
/// Held behind an [`Rc`] shared by the linker and every closure linked from it, so the
/// finalizer runs when the last holder drops — closing the "delete the linker while an
/// instance still calls into it" hazard without asking the embedder to order their
/// teardown.
struct EnvBox {
    env: *mut c_void,
    finalizer: Option<unsafe extern "C" fn(*mut c_void)>,
}

impl Drop for EnvBox {
    fn drop(&mut self) {
        if let Some(f) = self.finalizer {
            #[allow(unsafe_code, reason = "invoking the caller's own finalizer")]
            // SAFETY: `f` and `env` were supplied together by the caller, who asserted the
            // pairing when registering them. This runs exactly once — `EnvBox` is not
            // `Clone`, so only the final `Rc` drop reaches here.
            unsafe {
                f(self.env);
            }
        }
    }
}

pub struct wasmrt_linker {
    inner: Linker,
    /// Present once `wasmrt_linker_define_wasi` has been called, so `proc_exit`'s code can
    /// be read back.
    wasi: Option<wasmrt_core::wasi::SharedCtx>,
}

// =====================================================================================
// WASI config
// =====================================================================================

#[derive(Default)]
pub struct wasmrt_wasi_config {
    inherit_stdout: bool,
    inherit_stderr: bool,
    inherit_stdin: bool,
    args: Vec<Vec<u8>>,
    env: Vec<(Vec<u8>, Vec<u8>)>,
    preopens: Vec<(String, String, bool)>,
}

// =====================================================================================
// Caller
// =====================================================================================

/// The payload behind the opaque `wasmrt_caller_t *`.
///
/// It borrows the engine's `Caller`, so it carries lifetimes C cannot name — which is why
/// it crosses the boundary as an opaque pointer cast rather than a `Box`. It lives on the
/// stack frame that invokes the callback and is gone when that frame returns, which is
/// exactly the validity window `wasmrt.h` documents.
struct CallerCtx<'a, 'b> {
    caller: &'a mut Caller<'b>,
    /// The memory index to use for the convenience read/write helpers. Memory 0 is the
    /// only one a preview-1-era host surface addresses.
    memory: u32,
}

// =====================================================================================
// Exported functions
// =====================================================================================

/// Every export below is `#[unsafe(no_mangle)]`. Edition 2024 makes that an unsafe
/// attribute because it asserts the symbol name is unique across the final link — an
/// obligation discharged for all of them at once by the `wasmrt_` prefix, which is this
/// crate's whole naming convention. It is not a memory-safety `unsafe` and dereferences
/// nothing.
macro_rules! capi {
    ($(#[$m:meta])* fn $name:ident($($arg:ident : $ty:ty),* $(,)?) $(-> $ret:ty)? $body:block) => {
        $(#[$m])*
        #[allow(unsafe_code, reason = "no_mangle: symbol-name uniqueness, held by the wasmrt_ prefix")]
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $ty),*) $(-> $ret)? $body
    };
}

// ---- version ------------------------------------------------------------------------

capi! {
    /// The wasmrt C-ABI version. Compare against `WASMRT_ABI_VERSION` in `wasmrt.h`.
    fn wasmrt_abi_version() -> u32 {
        wasmrt_core::abi_version()
    }
}

capi! {
    /// The runtime version string, static storage.
    fn wasmrt_version_string() -> *const c_char {
        VERSION_CSTR.with(|v| v.as_ptr())
    }
}

thread_local! {
    static VERSION_CSTR: CString = cstring(wasmrt_core::VERSION);
}

// ---- config -------------------------------------------------------------------------

capi! {
    fn wasmrt_config_new() -> *mut wasmrt_config {
        ffi::into_raw(wasmrt_config {
            features: Features::all(),
            limits: ResourceLimits::defaults(),
        })
    }
}

capi! {
    fn wasmrt_config_delete(p: *mut wasmrt_config) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

capi! {
    fn wasmrt_config_set_feature(p: *mut wasmrt_config, f: u32, enabled: bool) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(cfg) = (unsafe { ffi::opt_mut(p) }) else { return false };
        let Some(feat) = feature_of(f) else { return false };
        cfg.features.set(feat, enabled);
        true
    }
}

capi! {
    fn wasmrt_config_get_feature(p: *const wasmrt_config, f: u32, out: *mut bool) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(cfg) = (unsafe { ffi::opt_ref(p) }) else { return false };
        let Some(feat) = feature_of(f) else { return false };
        #[allow(unsafe_code, reason = "writing a caller out-parameter via the ffi primitive")]
        unsafe { ffi::out(out, cfg.features.has(feat)) }
    }
}

capi! {
    fn wasmrt_config_all_features(p: *mut wasmrt_config, enabled: bool) {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(cfg) = (unsafe { ffi::opt_mut(p) }) else { return };
        cfg.features = if enabled { Features::all() } else { Features::mvp() };
    }
}

/// The limit setters share one shape: ignore 0 (documented as "leave unchanged") and
/// saturate to `usize` so a 64-bit ceiling on a 32-bit host cannot wrap into a small one.
macro_rules! limit_setter {
    ($name:ident, $field:ident, $ty:ty) => {
        capi! {
            fn $name(p: *mut wasmrt_config, v: $ty) {
                #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
                let Some(cfg) = (unsafe { ffi::opt_mut(p) }) else { return };
                if v == 0 {
                    return;
                }
                cfg.limits.$field = usize::try_from(v).unwrap_or(usize::MAX);
            }
        }
    };
}

limit_setter!(wasmrt_config_set_max_memory_bytes, max_memory_bytes, u64);
limit_setter!(wasmrt_config_set_max_table_elements, max_table_elems, u64);
limit_setter!(wasmrt_config_set_max_gc_objects, max_gc_objects, u64);
limit_setter!(wasmrt_config_set_max_exception_boxes, max_exn_boxes, u64);
limit_setter!(wasmrt_config_set_max_call_depth, max_call_depth, u32);

// ---- engine -------------------------------------------------------------------------

capi! {
    fn wasmrt_engine_new() -> *mut wasmrt_engine {
        ffi::into_raw(wasmrt_engine {
            features: Features::all(),
            limits: ResourceLimits::defaults(),
        })
    }
}

capi! {
    fn wasmrt_engine_new_with_config(
        p: *const wasmrt_config,
        error: *mut *mut wasmrt_error,
    ) -> *mut wasmrt_engine {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(cfg) = (unsafe { ffi::opt_ref(p) }) else {
            #[allow(unsafe_code, reason = "writing a caller out-parameter")]
            unsafe { ffi::out(error, err("wasmrt_engine_new_with_config: config is NULL")) };
            return core::ptr::null_mut();
        };
        // Incoherent sets are reported, never repaired: silently enabling a dependency
        // would accept modules the embedder meant to refuse.
        if let Err(e) = cfg.features.check_coherent() {
            #[allow(unsafe_code, reason = "writing a caller out-parameter")]
            unsafe { ffi::out(error, err(format!("invalid configuration: {e}"))) };
            return core::ptr::null_mut();
        }
        ffi::into_raw(wasmrt_engine {
            features: cfg.features,
            limits: cfg.limits,
        })
    }
}

capi! {
    fn wasmrt_engine_delete(p: *mut wasmrt_engine) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

// ---- store --------------------------------------------------------------------------

capi! {
    fn wasmrt_store_new(p: *mut wasmrt_engine) -> *mut wasmrt_store {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(e) = (unsafe { ffi::opt_ref(p.cast_const()) }) else {
            return core::ptr::null_mut();
        };
        ffi::into_raw(wasmrt_store {
            tag: NEXT_STORE_TAG.fetch_add(1, Ordering::Relaxed),
            inner: Store::with_limits(e.limits),
            instances: Vec::new(),
            funcs: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
        })
    }
}

capi! {
    fn wasmrt_store_delete(p: *mut wasmrt_store) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

// ---- handle validity ----------------------------------------------------------------

macro_rules! validity {
    ($name:ident, $ty:ty, $lookup:ident) => {
        capi! {
            fn $name(p: *const wasmrt_store, h: $ty) -> bool {
                #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
                let Some(s) = (unsafe { ffi::opt_ref(p) }) else { return false };
                s.$lookup(h).is_some()
            }
        }
    };
}

validity!(wasmrt_instance_is_valid, wasmrt_instance_t, instance);
validity!(wasmrt_func_is_valid, wasmrt_func_t, func);
validity!(wasmrt_memory_is_valid, wasmrt_memory_t, memory);
validity!(wasmrt_global_is_valid, wasmrt_global_t, global);

// ---- module -------------------------------------------------------------------------

capi! {
    fn wasmrt_module_new(
        ep: *mut wasmrt_engine,
        bytes: *const u8,
        len: usize,
        out: *mut *mut wasmrt_module,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(e) = (unsafe { ffi::opt_ref(ep.cast_const()) }) else {
            return err("wasmrt_module_new: engine is NULL");
        };
        #[allow(unsafe_code, reason = "borrowing a caller-sized byte range")]
        let Some(buf) = (unsafe { ffi::slice(bytes, len) }) else {
            return err("wasmrt_module_new: bytes is NULL");
        };
        let md = match wasmrt_core::module::decode(buf) {
            Ok(m) => m,
            Err(d) => return err(format!("decode failed: {d}")),
        };
        if let Err(v) = wasmrt_core::validate::validate_with_features(&md, &e.features) {
            return err(format!("{v}"));
        }
        let export_names = md.exports.iter().map(|x| cstring(&x.name)).collect();
        let import_modules = md.imports.iter().map(|x| cstring(&x.module)).collect();
        let import_names = md.imports.iter().map(|x| cstring(&x.name)).collect();
        let m = ffi::into_raw(wasmrt_module {
            md,
            export_names,
            import_modules,
            import_names,
        });
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        if !unsafe { ffi::out(out, m) } {
            // The caller gave us nowhere to put it; do not leak.
            #[allow(unsafe_code, reason = "reclaiming the module we just created")]
            unsafe { ffi::reclaim(m) };
            return err("wasmrt_module_new: out is NULL");
        }
        core::ptr::null_mut()
    }
}

capi! {
    fn wasmrt_module_validate(ep: *mut wasmrt_engine, bytes: *const u8, len: usize) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(e) = (unsafe { ffi::opt_ref(ep.cast_const()) }) else { return false };
        #[allow(unsafe_code, reason = "borrowing a caller-sized byte range")]
        let Some(buf) = (unsafe { ffi::slice(bytes, len) }) else { return false };
        wasmrt_core::module::decode(buf)
            .ok()
            .is_some_and(|md| {
                wasmrt_core::validate::validate_with_features(&md, &e.features).is_ok()
            })
    }
}

capi! {
    fn wasmrt_module_delete(p: *mut wasmrt_module) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

capi! {
    fn wasmrt_module_export_count(p: *const wasmrt_module) -> usize {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        (unsafe { ffi::opt_ref(p) }).map_or(0, |m| m.md.exports.len())
    }
}

capi! {
    fn wasmrt_module_export(
        p: *const wasmrt_module,
        i: usize,
        name_out: *mut *const c_char,
        name_len_out: *mut usize,
        kind_out: *mut wasmrt_externkind_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(m) = (unsafe { ffi::opt_ref(p) }) else { return false };
        let Some(e) = m.md.exports.get(i) else { return false };
        let Some(n) = m.export_names.get(i) else { return false };
        #[allow(unsafe_code, reason = "writing caller out-parameters")]
        unsafe {
            ffi::out(name_out, n.as_ptr());
            ffi::out(name_len_out, e.name.len());
            ffi::out(kind_out, e.ty.kind().into());
        }
        true
    }
}

capi! {
    fn wasmrt_module_import_count(p: *const wasmrt_module) -> usize {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        (unsafe { ffi::opt_ref(p) }).map_or(0, |m| m.md.imports.len())
    }
}

capi! {
    fn wasmrt_module_import(
        p: *const wasmrt_module,
        i: usize,
        module_out: *mut *const c_char,
        module_len_out: *mut usize,
        name_out: *mut *const c_char,
        name_len_out: *mut usize,
        kind_out: *mut wasmrt_externkind_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(m) = (unsafe { ffi::opt_ref(p) }) else { return false };
        let Some(imp) = m.md.imports.get(i) else { return false };
        let (Some(cm), Some(cn)) = (m.import_modules.get(i), m.import_names.get(i)) else {
            return false;
        };
        #[allow(unsafe_code, reason = "writing caller out-parameters")]
        unsafe {
            ffi::out(module_out, cm.as_ptr());
            ffi::out(module_len_out, imp.module.len());
            ffi::out(name_out, cn.as_ptr());
            ffi::out(name_len_out, imp.name.len());
            ffi::out(kind_out, imp.ty.kind().into());
        }
        true
    }
}

// ---- functype -----------------------------------------------------------------------

capi! {
    fn wasmrt_functype_new(
        params: *const wasmrt_valkind_t,
        nparams: usize,
        results: *const wasmrt_valkind_t,
        nresults: usize,
    ) -> *mut wasmrt_functype {
        fn read(p: *const wasmrt_valkind_t, n: usize) -> Option<Vec<wasmrt_valkind_t>> {
            if n == 0 {
                return Some(Vec::new());
            }
            if p.is_null() {
                return None;
            }
            #[allow(unsafe_code, reason = "borrowing a caller-sized array")]
            // SAFETY: non-null checked; `n` is the caller's own count, paired with `p` in
            // the same call as `wasmrt.h` requires.
            let s = unsafe { core::slice::from_raw_parts(p, n) };
            Some(s.to_vec())
        }
        let (Some(params), Some(results)) = (read(params, nparams), read(results, nresults))
        else {
            return core::ptr::null_mut();
        };
        ffi::into_raw(wasmrt_functype { params, results })
    }
}

capi! {
    fn wasmrt_functype_delete(p: *mut wasmrt_functype) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

capi! {
    fn wasmrt_functype_param_count(p: *const wasmrt_functype) -> usize {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        (unsafe { ffi::opt_ref(p) }).map_or(0, |t| t.params.len())
    }
}

capi! {
    fn wasmrt_functype_result_count(p: *const wasmrt_functype) -> usize {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        (unsafe { ffi::opt_ref(p) }).map_or(0, |t| t.results.len())
    }
}

capi! {
    fn wasmrt_functype_param(
        p: *const wasmrt_functype,
        i: usize,
        out: *mut wasmrt_valkind_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(t) = (unsafe { ffi::opt_ref(p) }) else { return false };
        let Some(&k) = t.params.get(i) else { return false };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, k) }
    }
}

capi! {
    fn wasmrt_functype_result(
        p: *const wasmrt_functype,
        i: usize,
        out: *mut wasmrt_valkind_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(t) = (unsafe { ffi::opt_ref(p) }) else { return false };
        let Some(&k) = t.results.get(i) else { return false };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, k) }
    }
}

// ---- caller -------------------------------------------------------------------------

pub type wasmrt_func_callback_t = Option<
    unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        *const wasmrt_val_t,
        usize,
        *mut wasmrt_val_t,
        usize,
    ) -> *mut wasmrt_trap,
>;

capi! {
    fn wasmrt_caller_get_memory(
        p: *mut c_void,
        _name: *const c_char,
        out: *mut wasmrt_memory_t,
    ) -> bool {
        // A caller handle names a memory only for the duration of the call, and the store
        // it belongs to is mid-borrow — so there is no live `wasmrt_store_t` to tag a
        // durable handle against. Callbacks use the read/write helpers below instead; this
        // exists so the wasmtime-shaped call sequence compiles, and reports honestly that
        // it produced nothing.
        #[allow(unsafe_code, reason = "borrowing the callback's caller context")]
        let Some(_ctx) = (unsafe { ffi::downcast::<CallerCtx<'_, '_>>(p) }) else { return false };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, wasmrt_memory_t { id: 0 }) };
        false
    }
}

capi! {
    fn wasmrt_caller_read(p: *mut c_void, offset: u64, dst: *mut c_void, n: usize) -> bool {
        #[allow(unsafe_code, reason = "borrowing the callback's caller context")]
        let Some(ctx) = (unsafe { ffi::downcast::<CallerCtx<'_, '_>>(p) }) else { return false };
        let Some(src) = ctx.caller.read(ctx.memory, offset, n) else { return false };
        #[allow(unsafe_code, reason = "copying into the caller's buffer")]
        unsafe { ffi::copy_out(src, dst) }
    }
}

capi! {
    fn wasmrt_caller_write(p: *mut c_void, offset: u64, src: *const c_void, n: usize) -> bool {
        #[allow(unsafe_code, reason = "borrowing the callback's caller context")]
        let Some(ctx) = (unsafe { ffi::downcast::<CallerCtx<'_, '_>>(p) }) else { return false };
        let Some(dst) = ctx.caller.write(ctx.memory, offset, n) else { return false };
        #[allow(unsafe_code, reason = "copying from the caller's buffer")]
        unsafe { ffi::copy_in(src, n, dst) }
    }
}

capi! {
    fn wasmrt_caller_memory_size(p: *mut c_void) -> usize {
        #[allow(unsafe_code, reason = "borrowing the callback's caller context")]
        let Some(ctx) = (unsafe { ffi::downcast::<CallerCtx<'_, '_>>(p) }) else { return 0 };
        ctx.caller.memory_len(ctx.memory).unwrap_or(0)
    }
}

// ---- linker -------------------------------------------------------------------------

capi! {
    fn wasmrt_linker_new(ep: *mut wasmrt_engine) -> *mut wasmrt_linker {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(_e) = (unsafe { ffi::opt_ref(ep.cast_const()) }) else {
            return core::ptr::null_mut();
        };
        ffi::into_raw(wasmrt_linker {
            inner: Linker::new(),
            wasi: None,
        })
    }
}

capi! {
    fn wasmrt_linker_delete(p: *mut wasmrt_linker) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

capi! {
    fn wasmrt_linker_define_func(
        p: *mut wasmrt_linker,
        module: *const c_char,
        name: *const c_char,
        ty: *const wasmrt_functype,
        cb: wasmrt_func_callback_t,
        env: *mut c_void,
        finalizer: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (l, m, n, t) = unsafe {
            (ffi::opt_mut(p), ffi::cstr(module), ffi::cstr(name), ffi::opt_ref(ty))
        };
        let (Some(l), Some(m), Some(n), Some(t)) = (l, m, n, t) else {
            return err("wasmrt_linker_define_func: a required argument was NULL or not UTF-8");
        };
        let Some(cb) = cb else {
            return err("wasmrt_linker_define_func: callback is NULL");
        };

        // The environment is shared with the closure so its finalizer runs when the LAST
        // holder drops — deleting the linker while an instance still calls in is safe.
        let boxed = Rc::new(EnvBox { env, finalizer });
        let param_kinds = t.params.clone();
        let result_kinds = t.results.clone();

        l.inner.define_func(m, n, move |caller, args, results| {
            let mut in_vals: Vec<wasmrt_val_t> = Vec::with_capacity(args.len());
            for (i, &a) in args.iter().enumerate() {
                let k = param_kinds.get(i).copied().unwrap_or(wasmrt_valkind_t::I64);
                in_vals.push(from_value(k, a));
            }
            let mut out_vals: Vec<wasmrt_val_t> = result_kinds
                .iter()
                .map(|&k| from_value(k, 0))
                .collect();
            // Sized to the DECLARED arity, which the engine also sized `results` to; if a
            // module declared a different one the engine would have refused to link.
            let mut ctx = CallerCtx { caller, memory: 0 };
            let ctx_ptr: *mut c_void = (&raw mut ctx).cast();

            #[allow(unsafe_code, reason = "invoking the caller's own callback")]
            // SAFETY: `cb` is the function pointer the caller registered, invoked with the
            // signature `wasmrt.h` declares for it. `ctx_ptr` points at a local live for
            // this call, which is the documented validity window.
            let trap = unsafe {
                cb(
                    boxed.env,
                    ctx_ptr,
                    in_vals.as_ptr(),
                    in_vals.len(),
                    out_vals.as_mut_ptr(),
                    out_vals.len(),
                )
            };

            if !trap.is_null() {
                #[allow(unsafe_code, reason = "taking ownership of the trap the callback returned")]
                let t = unsafe { ffi::reclaim(trap) };
                // Park the message so the call site can report it: core's `Trap` is a
                // closed enum and would otherwise lose the text.
                if let Some(t) = t {
                    HOST_TRAP.with_borrow_mut(|slot| *slot = Some(t.message.clone()));
                }
                return Err(Trap::HostTrap);
            }

            for (i, slot) in results.iter_mut().enumerate() {
                if let Some(v) = out_vals.get(i) {
                    *slot = to_value(v);
                }
            }
            Ok(())
        });
        core::ptr::null_mut()
    }
}

capi! {
    fn wasmrt_linker_define_global(
        p: *mut wasmrt_linker,
        module: *const c_char,
        name: *const c_char,
        value: wasmrt_val_t,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (l, m, n) = unsafe { (ffi::opt_mut(p), ffi::cstr(module), ffi::cstr(name)) };
        let (Some(l), Some(m), Some(n)) = (l, m, n) else {
            return err("wasmrt_linker_define_global: a required argument was NULL or not UTF-8");
        };
        l.inner.define_global(m, n, to_value(&value));
        core::ptr::null_mut()
    }
}

capi! {
    fn wasmrt_linker_define_instance(
        p: *mut wasmrt_linker,
        sp: *mut wasmrt_store,
        module: *const c_char,
        instance: wasmrt_instance_t,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (l, s, m) = unsafe {
            (ffi::opt_mut(p), ffi::opt_ref(sp.cast_const()), ffi::cstr(module))
        };
        let (Some(l), Some(s), Some(m)) = (l, s, m) else {
            return err("wasmrt_linker_define_instance: a required argument was NULL or not UTF-8");
        };
        let Some(id) = s.instance(instance) else {
            return err("wasmrt_linker_define_instance: instance handle does not belong to this store");
        };
        l.inner.define_instance(m, id);
        core::ptr::null_mut()
    }
}

capi! {
    fn wasmrt_linker_define_wasi(
        p: *mut wasmrt_linker,
        cp: *const wasmrt_wasi_config,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (l, c) = unsafe { (ffi::opt_mut(p), ffi::opt_ref(cp)) };
        let (Some(l), Some(c)) = (l, c) else {
            return err("wasmrt_linker_define_wasi: a required argument was NULL");
        };

        // A CSPRNG that cannot be seeded must fail loudly rather than run predictably —
        // the one failure mode that turns a CSPRNG into a security hole.
        let Some(ctx) = wasmrt_core::wasi::WasiCtx::new() else {
            return err("no OS entropy available; refusing to provide a predictable RNG");
        };
        let mut ctx = ctx.with_args(c.args.iter()).with_env(
            c.env.iter().map(|(k, v)| (k.clone(), v.clone())),
        );
        for (host, guest, ro) in &c.preopens {
            let rights = if *ro {
                wasmrt_core::wasi::fs::rights::READ_ONLY
            } else {
                wasmrt_core::wasi::fs::rights::ALL
            };
            if let Err(e) = ctx.preopen_dir(std::path::Path::new(host), guest, rights) {
                return err(format!("cannot preopen {host}: errno {e}"));
            }
        }
        let shared = wasmrt_core::wasi::shared(ctx);
        // Namespaced, NOT a fallback: an embedder who also defines `env.foo` should still
        // get a link error for a typo, rather than a WASI stub answering NOSYS at runtime.
        wasmrt_core::wasi::define_namespaces(&mut l.inner, &shared);
        l.wasi = Some(shared);
        core::ptr::null_mut()
    }
}

capi! {
    fn wasmrt_linker_define_unknown_imports_as_traps(p: *mut wasmrt_linker) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(l) = (unsafe { ffi::opt_mut(p) }) else {
            return err("wasmrt_linker_define_unknown_imports_as_traps: linker is NULL");
        };
        l.inner.define_fallback(|module, name| {
            let what = format!("unimplemented import `{module}`.`{name}` was called");
            host_func(move |_c, _a, _r| {
                HOST_TRAP.with_borrow_mut(|slot| *slot = Some(cstring(&what)));
                Err(Trap::HostTrap)
            })
        });
        core::ptr::null_mut()
    }
}

capi! {
    fn wasmrt_linker_instantiate(
        p: *mut wasmrt_linker,
        sp: *mut wasmrt_store,
        mp: *const wasmrt_module,
        out: *mut wasmrt_instance_t,
        trap_out: *mut *mut wasmrt_trap,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (l, s, m) = unsafe { (ffi::opt_mut(p), ffi::opt_mut(sp), ffi::opt_ref(mp)) };
        let (Some(l), Some(s), Some(m)) = (l, s, m) else {
            return err("wasmrt_linker_instantiate: a required argument was NULL");
        };
        let imports = match l.inner.resolve(&s.inner, &m.md) {
            Ok(i) => i,
            Err(e) => return err(link_error_text(&e)),
        };
        match s.inner.instantiate(m.md.clone(), imports) {
            Ok(id) => {
                s.instances.push(id);
                let h = wasmrt_instance_t {
                    id: pack(s.tag, s.instances.len() - 1),
                };
                #[allow(unsafe_code, reason = "writing a caller out-parameter")]
                unsafe { ffi::out(out, h) };
                core::ptr::null_mut()
            }
            Err(t) => {
                // An instantiation trap (a start function that trapped, a segment that did
                // not fit) is a TRAP, not a host error — reported through `trap_out` so the
                // caller can tell "the guest misbehaved" from "linking was wrong".
                let msg = trap_message(&t);
                #[allow(unsafe_code, reason = "writing a caller out-parameter")]
                unsafe { ffi::out(trap_out, ffi::into_raw(wasmrt_trap { message: msg })) };
                core::ptr::null_mut()
            }
        }
    }
}

capi! {
    fn wasmrt_wasi_exit_code(p: *const wasmrt_linker, out: *mut i32) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(l) = (unsafe { ffi::opt_ref(p) }) else { return false };
        let Some(ctx) = l.wasi.as_ref() else { return false };
        let Some(code) = ctx.borrow().exit_code() else { return false };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, code) }
    }
}

// ---- WASI config --------------------------------------------------------------------

capi! {
    fn wasmrt_wasi_config_new() -> *mut wasmrt_wasi_config {
        ffi::into_raw(wasmrt_wasi_config::default())
    }
}

capi! {
    fn wasmrt_wasi_config_delete(p: *mut wasmrt_wasi_config) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

macro_rules! wasi_inherit {
    ($name:ident, $field:ident) => {
        capi! {
            fn $name(p: *mut wasmrt_wasi_config) {
                #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
                let Some(c) = (unsafe { ffi::opt_mut(p) }) else { return };
                c.$field = true;
            }
        }
    };
}

wasi_inherit!(wasmrt_wasi_config_inherit_stdout, inherit_stdout);
wasi_inherit!(wasmrt_wasi_config_inherit_stderr, inherit_stderr);
wasi_inherit!(wasmrt_wasi_config_inherit_stdin, inherit_stdin);

capi! {
    fn wasmrt_wasi_config_set_args(
        p: *mut wasmrt_wasi_config,
        argv: *const *const c_char,
        n: usize,
    ) {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(c) = (unsafe { ffi::opt_mut(p) }) else { return };
        c.args = read_cstr_array(argv, n);
    }
}

capi! {
    fn wasmrt_wasi_config_set_env(
        p: *mut wasmrt_wasi_config,
        names: *const *const c_char,
        values: *const *const c_char,
        n: usize,
    ) {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(c) = (unsafe { ffi::opt_mut(p) }) else { return };
        let ks = read_cstr_array(names, n);
        let vs = read_cstr_array(values, n);
        c.env = ks.into_iter().zip(vs).collect();
    }
}

capi! {
    fn wasmrt_wasi_config_preopen_dir(
        p: *mut wasmrt_wasi_config,
        host_path: *const c_char,
        guest_path: *const c_char,
        read_only: bool,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (c, h, g) = unsafe { (ffi::opt_mut(p), ffi::cstr(host_path), ffi::cstr(guest_path)) };
        let (Some(c), Some(h), Some(g)) = (c, h, g) else {
            return err("wasmrt_wasi_config_preopen_dir: a required argument was NULL or not UTF-8");
        };
        // Checked here so an obvious mistake is caught at config time; the real preopen
        // happens in `wasmrt_linker_define_wasi`, which reports its own failures.
        if !std::path::Path::new(h).is_dir() {
            return err(format!("not a directory: {h}"));
        }
        c.preopens.push((h.to_string(), g.to_string(), read_only));
        core::ptr::null_mut()
    }
}

/// Read `n` NUL-terminated strings from a C array of pointers, skipping any that are null
/// so one bad entry cannot lose the rest.
fn read_cstr_array(p: *const *const c_char, n: usize) -> Vec<Vec<u8>> {
    if n == 0 || p.is_null() {
        return Vec::new();
    }
    #[allow(unsafe_code, reason = "borrowing a caller-sized array of pointers")]
    // SAFETY: non-null checked; `n` is the caller's own count paired with `p`.
    let ptrs = unsafe { core::slice::from_raw_parts(p, n) };
    ptrs.iter()
        .map(|&q| {
            #[allow(unsafe_code, reason = "reading one caller string via the ffi primitive")]
            let s = unsafe { ffi::cstr(q) };
            s.map(|s| s.as_bytes().to_vec()).unwrap_or_default()
        })
        .collect()
}

// ---- instance exports ----------------------------------------------------------------

capi! {
    fn wasmrt_instance_get_func(
        sp: *mut wasmrt_store,
        inst: wasmrt_instance_t,
        name: *const c_char,
        out: *mut wasmrt_func_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (s, n) = unsafe { (ffi::opt_mut(sp), ffi::cstr(name)) };
        let (Some(s), Some(n)) = (s, n) else { return false };
        let Some(id) = s.instance(inst) else { return false };
        let Some(fi) = s.inner.export_func(id, n) else { return false };
        let Some(slot) = s.slot_of(id) else { return false };
        s.funcs.push((slot, fi));
        let h = wasmrt_func_t { id: pack(s.tag, s.funcs.len() - 1) };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, h) }
    }
}

capi! {
    fn wasmrt_instance_get_memory(
        sp: *mut wasmrt_store,
        inst: wasmrt_instance_t,
        name: *const c_char,
        out: *mut wasmrt_memory_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (s, n) = unsafe { (ffi::opt_mut(sp), ffi::cstr(name)) };
        let (Some(s), Some(n)) = (s, n) else { return false };
        let Some(id) = s.instance(inst) else { return false };
        let Some(mi) = s.inner.export_index(id, n, ExternKind::Memory) else { return false };
        let Some(slot) = s.slot_of(id) else { return false };
        s.memories.push((slot, mi));
        let h = wasmrt_memory_t { id: pack(s.tag, s.memories.len() - 1) };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, h) }
    }
}

capi! {
    fn wasmrt_instance_get_global(
        sp: *mut wasmrt_store,
        inst: wasmrt_instance_t,
        name: *const c_char,
        out: *mut wasmrt_global_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing caller handles via the ffi primitives")]
        let (s, n) = unsafe { (ffi::opt_mut(sp), ffi::cstr(name)) };
        let (Some(s), Some(n)) = (s, n) else { return false };
        let Some(id) = s.instance(inst) else { return false };
        let Some(gi) = s.inner.export_index(id, n, ExternKind::Global) else { return false };
        let Some(slot) = s.slot_of(id) else { return false };
        s.globals.push((slot, gi));
        let h = wasmrt_global_t { id: pack(s.tag, s.globals.len() - 1) };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, h) }
    }
}

capi! {
    fn wasmrt_instance_initialize(
        sp: *mut wasmrt_store,
        inst: wasmrt_instance_t,
        trap_out: *mut *mut wasmrt_trap,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_mut(sp) }) else {
            return err("wasmrt_instance_initialize: store is NULL");
        };
        let Some(id) = s.instance(inst) else {
            return err("wasmrt_instance_initialize: instance handle does not belong to this store");
        };
        // A command module has no `_initialize`; that is not an error.
        if s.inner.export_func(id, "_initialize").is_none() {
            return core::ptr::null_mut();
        }
        if let Err(t) = s.inner.invoke(id, "_initialize", &[]) {
            let msg = trap_message(&t);
            #[allow(unsafe_code, reason = "writing a caller out-parameter")]
            unsafe { ffi::out(trap_out, ffi::into_raw(wasmrt_trap { message: msg })) };
        }
        core::ptr::null_mut()
    }
}

// ---- calling ------------------------------------------------------------------------

capi! {
    fn wasmrt_func_type(sp: *const wasmrt_store, h: wasmrt_func_t) -> *mut wasmrt_functype {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_ref(sp) }) else { return core::ptr::null_mut() };
        let Some((id, fi)) = s.func(h) else { return core::ptr::null_mut() };
        let Some(ft) = s.inner.func_type(id, fi) else { return core::ptr::null_mut() };
        let (Some(params), Some(results)) = (kinds(&ft.params), kinds(&ft.results)) else {
            return core::ptr::null_mut(); // a signature this boundary cannot describe
        };
        ffi::into_raw(wasmrt_functype { params, results })
    }
}

/// Map a wasm signature to boundary kinds, or `None` if any type cannot cross.
fn kinds(ts: &[ValType]) -> Option<Vec<wasmrt_valkind_t>> {
    ts.iter().map(|&t| kind_of(t)).collect()
}

capi! {
    fn wasmrt_func_call(
        sp: *mut wasmrt_store,
        h: wasmrt_func_t,
        args: *const wasmrt_val_t,
        nargs: usize,
        results: *mut wasmrt_val_t,
        nresults: usize,
        trap_out: *mut *mut wasmrt_trap,
    ) -> *mut wasmrt_error {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_mut(sp) }) else {
            return err("wasmrt_func_call: store is NULL");
        };
        let Some((id, fi)) = s.func(h) else {
            return err("wasmrt_func_call: function handle does not belong to this store");
        };
        let Some(ft) = s.inner.func_type(id, fi) else {
            return err("wasmrt_func_call: no such function");
        };
        if ft.params.len() != nargs {
            return err(format!(
                "wasmrt_func_call: expected {} argument(s), got {nargs}",
                ft.params.len()
            ));
        }
        if ft.results.len() != nresults {
            return err(format!(
                "wasmrt_func_call: expected room for {} result(s), got {nresults}",
                ft.results.len()
            ));
        }
        let Some(result_kinds) = kinds(&ft.results) else {
            return err("wasmrt_func_call: this function returns a type the C boundary cannot carry");
        };
        if kinds(&ft.params).is_none() {
            return err("wasmrt_func_call: this function takes a type the C boundary cannot carry");
        }

        #[allow(unsafe_code, reason = "borrowing a caller-sized array")]
        let Some(in_slice) = (unsafe { arg_slice(args, nargs) }) else {
            return err("wasmrt_func_call: args is NULL");
        };
        let vals: Vec<Value> = in_slice.iter().map(to_value).collect();

        match s.inner.invoke_index(id, fi, &vals) {
            Ok(out) => {
                for (i, k) in result_kinds.iter().enumerate() {
                    let v = out.get(i).copied().unwrap_or(0);
                    #[allow(unsafe_code, reason = "writing one element of the caller's results array")]
                    // SAFETY: `i < nresults`, which was checked equal to the declared
                    // result count above, and `results` is the caller's array of that size.
                    unsafe {
                        if !results.is_null() {
                            core::ptr::write(results.add(i), from_value(*k, v));
                        }
                    }
                }
                core::ptr::null_mut()
            }
            Err(t) => {
                let msg = trap_message(&t);
                #[allow(unsafe_code, reason = "writing a caller out-parameter")]
                unsafe { ffi::out(trap_out, ffi::into_raw(wasmrt_trap { message: msg })) };
                core::ptr::null_mut()
            }
        }
    }
}

/// Borrow the caller's argument array.
///
/// # Safety
/// `p` must point to `n` initialized `wasmrt_val_t`.
#[allow(unsafe_code, reason = "borrowing a caller-sized array of values")]
unsafe fn arg_slice<'a>(p: *const wasmrt_val_t, n: usize) -> Option<&'a [wasmrt_val_t]> {
    if n == 0 {
        return Some(&[]);
    }
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null checked; `n` is the caller's own count, paired with `p`.
    Some(unsafe { core::slice::from_raw_parts(p, n) })
}

// ---- memory -------------------------------------------------------------------------

capi! {
    fn wasmrt_memory_data(sp: *mut wasmrt_store, h: wasmrt_memory_t) -> *mut u8 {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_mut(sp) }) else { return core::ptr::null_mut() };
        let Some((id, mi)) = s.memory(h) else { return core::ptr::null_mut() };
        match s.inner.memory_mut(id, mi) {
            Some(m) => m.bytes.as_mut_ptr(),
            None => core::ptr::null_mut(),
        }
    }
}

capi! {
    fn wasmrt_memory_data_size(sp: *const wasmrt_store, h: wasmrt_memory_t) -> usize {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_ref(sp) }) else { return 0 };
        let Some((id, mi)) = s.memory(h) else { return 0 };
        s.inner.memory(id, mi).map_or(0, |m| m.bytes.len())
    }
}

capi! {
    fn wasmrt_memory_size_pages(sp: *const wasmrt_store, h: wasmrt_memory_t) -> u64 {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_ref(sp) }) else { return 0 };
        let Some((id, mi)) = s.memory(h) else { return 0 };
        s.inner
            .memory(id, mi)
            .map_or(0, |m| (m.bytes.len() / interp::PAGE_SIZE) as u64)
    }
}

capi! {
    fn wasmrt_memory_read(
        sp: *const wasmrt_store,
        h: wasmrt_memory_t,
        offset: u64,
        dst: *mut c_void,
        n: usize,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_ref(sp) }) else { return false };
        let Some((id, mi)) = s.memory(h) else { return false };
        let Some(m) = s.inner.memory(id, mi) else { return false };
        let Some(src) = range(m.bytes.len(), offset, n) else { return false };
        #[allow(unsafe_code, reason = "copying into the caller's buffer")]
        unsafe { ffi::copy_out(&m.bytes[src], dst) }
    }
}

capi! {
    fn wasmrt_memory_write(
        sp: *mut wasmrt_store,
        h: wasmrt_memory_t,
        offset: u64,
        src: *const c_void,
        n: usize,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_mut(sp) }) else { return false };
        let Some((id, mi)) = s.memory(h) else { return false };
        let Some(m) = s.inner.memory_mut(id, mi) else { return false };
        let Some(r) = range(m.bytes.len(), offset, n) else { return false };
        #[allow(unsafe_code, reason = "copying from the caller's buffer")]
        unsafe { ffi::copy_in(src, n, &mut m.bytes[r]) }
    }
}

/// The byte range `[offset, offset+n)` if it fits entirely within `len`.
///
/// Overflow-checked before the comparison: `offset + n` wrapping would otherwise turn an
/// out-of-bounds request into an in-bounds one.
fn range(len: usize, offset: u64, n: usize) -> Option<core::ops::Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(n)?;
    (end <= len).then_some(start..end)
}

// ---- globals ------------------------------------------------------------------------

capi! {
    fn wasmrt_global_get(
        sp: *const wasmrt_store,
        h: wasmrt_global_t,
        out: *mut wasmrt_val_t,
    ) -> bool {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        let Some(s) = (unsafe { ffi::opt_ref(sp) }) else { return false };
        let Some((id, gi)) = s.global(h) else { return false };
        let Some(v) = s.inner.global(id, gi) else { return false };
        // `module_of` is fallible now (an `InstanceId` carries its issuing store), and the honest
        // handling here is the one this function already uses everywhere: report `false` rather than
        // unwrap. The handle's store was checked above, so `None` is unreachable in practice — but
        // "unreachable" is exactly the reasoning this crate has been burned by before.
        let Some(md) = s.inner.module_of(id) else { return false };
        let Some(ty) = md.globals.get(gi as usize) else { return false };
        let Some(kind) = kind_of(ty.content) else { return false };
        #[allow(unsafe_code, reason = "writing a caller out-parameter")]
        unsafe { ffi::out(out, from_value(kind, v)) }
    }
}

// ---- traps and errors ----------------------------------------------------------------

capi! {
    /// Create a trap for a host callback to return. Returning it to the engine transfers
    /// ownership; a caller must delete only traps the engine gave *them*.
    fn wasmrt_trap_new(message: *const c_char) -> *mut wasmrt_trap {
        #[allow(unsafe_code, reason = "reading the caller's message via the ffi primitive")]
        let m = unsafe { ffi::cstr(message) };
        trap_obj(m.unwrap_or("host trap"))
    }
}

capi! {
    fn wasmrt_trap_message(p: *const wasmrt_trap) -> *const c_char {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        (unsafe { ffi::opt_ref(p) }).map_or(core::ptr::null(), |t| t.message.as_ptr())
    }
}

capi! {
    fn wasmrt_trap_delete(p: *mut wasmrt_trap) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

capi! {
    /// Always 0 in this release — see the header. Per-instruction byte offsets are not
    /// recorded yet, so there is nothing truthful to report, and an approximate frame is
    /// worse than none.
    fn wasmrt_trap_frame_count(_p: *const wasmrt_trap) -> usize {
        0
    }
}

capi! {
    fn wasmrt_trap_frame(
        _p: *const wasmrt_trap,
        _i: usize,
        _func_index_out: *mut u32,
        _offset_out: *mut u32,
        _name_out: *mut *const c_char,
    ) -> bool {
        false
    }
}

capi! {
    fn wasmrt_error_message(p: *const wasmrt_error) -> *const c_char {
        #[allow(unsafe_code, reason = "borrowing a caller handle via the ffi primitive")]
        (unsafe { ffi::opt_ref(p) }).map_or(core::ptr::null(), |e| e.message.as_ptr())
    }
}

capi! {
    fn wasmrt_error_delete(p: *mut wasmrt_error) {
        #[allow(unsafe_code, reason = "reclaiming a pointer this crate handed out")]
        unsafe { ffi::reclaim(p) };
    }
}

#[cfg(test)]
mod tests;
