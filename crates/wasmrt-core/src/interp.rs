//! Execution: instantiate a validated module and run its functions with a switch-dispatched
//! interpreter over the `opcode` IR (Option A — see `cmem/architecture.md`).
//!
//! Ported from wazmrt `src/interp.zig` (T5). Values are untyped `u64` slots — validation has
//! proven the types, so the stack carries raw bits and each opcode reinterprets. Control flow
//! uses a per-call label stack + a branch-target table precomputed once per function (matching
//! `end`/`else` for every `block`/`loop`/`if`).
//!
//! **Scope this release (v0.6.0): integer compute.** i32/i64 arithmetic/comparison/bitwise,
//! structured control flow, direct `call` (incl. recursion), locals, globals, and constants —
//! enough to run a compute module (`fib`, `factorial`, `add`). Float arithmetic and linear
//! memory land in 0.6.1; tables, reference types, GC, SIMD, threads, and exception handling in
//! later 0.6.x slices. **Anything not yet executed traps loudly** ([`Trap::UnsupportedInstruction`]),
//! never silent-wrong. Host imports link through [`Imports`], as do imported **memories**
//! (shared with the exporting instance, never copied); imported **tables** still reject loudly
//! ([`Trap::UnsupportedImportKind`]) until a `funcref` carries its owning instance.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::module::{CompKind, CompType, FuncType, Module, StorageType};
use crate::opcode::{BlockType, Catch, CatchKind, HeapType, Imm, Instr, Op, RefType};
use crate::reader::Reader;
use crate::types::{DecodeError, RefHeap};

/// A runtime value: a raw 128-bit slot reinterpreted per the (validated) type.
///
/// The slot is 128 bits wide so a `v128` (SIMD) fits in a **single** stack/local/table/global
/// entry — keeping the whole interpreter on the "one slot per value" model (arity, `select`,
/// `drop`, branch copies, locals, and call marshaling never have to reason about slot width).
/// This is an idiomatic-Rust divergence from wazmrt, which stores a `v128` as two `u64` slots
/// and carries width tables to size `drop`/`select`; observable behavior is identical. Scalars
/// (`i32`/`i64`/`f32`/`f64`) and references live in the low bits, high 64 zero.
pub type Value = u128;

#[must_use]
pub fn i32_value(x: i32) -> Value {
    Value::from(x as u32)
}
#[must_use]
pub fn as_i32(v: Value) -> i32 {
    v as u32 as i32
}
#[must_use]
pub fn i64_value(x: i64) -> Value {
    Value::from(x as u64)
}
#[must_use]
pub fn as_i64(v: Value) -> i64 {
    v as u64 as i64
}
#[must_use]
pub fn f32_value(x: f32) -> Value {
    Value::from(x.to_bits())
}
#[must_use]
pub fn as_f32(v: Value) -> f32 {
    f32::from_bits(v as u32)
}
#[must_use]
pub fn f64_value(x: f64) -> Value {
    Value::from(x.to_bits())
}
#[must_use]
pub fn as_f64(v: Value) -> f64 {
    f64::from_bits(v as u64)
}

/// Default cap on guest call depth (a `call` recurses natively, so this bounds host stack
/// use). **512 matches the frozen oracle exactly** — do not change the default to work
/// around the debug-build stack finding in `cmem/known-issues.md`; lower it per-store
/// through [`ResourceLimits::max_call_depth`] instead.
const DEFAULT_MAX_CALL_DEPTH: usize = 512;

/// WebAssembly linear-memory page size (64 KiB).
pub const PAGE_SIZE: usize = 64 * 1024;

/// Default ceiling on total linear memory per instance (summed across memories), applied at
/// instantiation and at `memory.grow`. A tiny module can declare gigabytes, so this bounds a
/// hostile input. 1 GiB is far above any realistic guest.
const DEFAULT_MAX_MEMORY_BYTES: usize = 1 << 30;

/// Default ceiling on total defined-table entries per instance. Each entry is an eagerly
/// allocated `Value`, so a tiny module's unvalidated `min` (up to 2^32-1) could otherwise
/// demand tens of GiB. 2^27 (~1 GiB of slots) is far above any realistic guest.
const DEFAULT_MAX_TABLE_ELEMS: usize = 1 << 27;

/// The null-reference sentinel — a value-stack `ref.null`, and an uninitialized table entry.
/// A funcref value is a (small) function index, so it never collides.
///
/// **Invariant (do not drift):** `null_ref` (all bits set) is checked *before* [`I31_TAG`]
/// (bit 63), so the two never confuse (`cmem/design-decisions.md`).
pub const NULL_REF: Value = u64::MAX as Value;

/// Tag bit marking a value slot as an unboxed i31 (WasmGC). Set on `ref.i31` results so
/// `ref.test`/`ref.cast` can tell an i31 from a heap-object index (bit 63 clear) within the
/// `any` hierarchy. Checked *after* [`NULL_REF`] (all bits set), so the two never confuse.
const I31_TAG: Value = 1 << 63;
const _: () = assert!(I31_TAG == (1u128 << 63)); // i31 tag lives in the low 64 bits

// --- the externref bridge: HOST_TAG and EXTERN_TAG (S1) -------------------------------
//
// Before this existed, `any.convert_extern` / `extern.convert_any` were **deliberately absent**
// from the decoder, because an `externref` and a GC object shared one numeric space: a host
// handle crossing the C ABI is a raw `uint64_t`, while `ref_matches`' `Any` arm reads a
// non-null, non-i31 reference as a `gc_heap` INDEX. Convert one to the other and host handle 2
// reads as GC object #2 — a type-confused read that the cast believes it verified.
//
// The fix is a representation, not two opcode arms. Every reference form lived in the LOW 64
// bits of the `u128` slot, so the high half was free for exactly the two facts the low half
// could not carry:
//
// ```text
//   bit 65      bit 64     bits 63..0
//   extern      host       the internal reference (null / i31 / funcref / gc index / host addr)
// ```
//
// * [`HOST_TAG`] marks the low bits as a **host address** rather than a GC heap index. That is
//   what makes the two spaces distinct, and it is the half that closes the type confusion.
// * [`EXTERN_TAG`] marks the value as an **`externref` wrapper** around whatever the remaining
//   bits describe. `extern.convert_any` sets it, `any.convert_extern` clears it — the spec's
//   model exactly, where `(ref.extern n)` *is* `extern.convert_any (ref.host n)`.
//
// ⚠️ A `v128` uses the whole 128-bit slot, so these bits are free only for values validation has
// proven to be references. That is the same contract [`I31_TAG`] already relies on.
//
// ⚠️ **`NULL_REF` never carries either tag.** Null externalizes and internalizes to null
// (§4.4.7.3), so `ref.is_null` stays one `==` against the sentinel and every existing null check
// keeps working untouched.

/// Marks the low bits as a **host address** (`ref.host n`) rather than a GC heap index.
///
/// A host reference is an `anyref` and nothing narrower — in particular **not `eqref`**, which
/// `ref_test.wast` pins directly (index 6, `any.convert_extern` of a host externref, answers 2
/// for `any` and 0 for `eq`).
pub const HOST_TAG: Value = 1 << 64;

/// Marks the value as an **`externref` wrapper** around the internal reference in the remaining
/// bits. Set by `extern.convert_any`, cleared by `any.convert_extern`.
pub const EXTERN_TAG: Value = 1 << 65;

const _: () = assert!(HOST_TAG > u64::MAX as Value); // both tags live ABOVE the low 64 bits
const _: () = assert!(EXTERN_TAG > u64::MAX as Value);

/// The value of `(ref.host n)`: an `anyref` naming host address `n`.
#[must_use]
pub fn host_ref(n: u64) -> Value {
    HOST_TAG | Value::from(n)
}

/// `extern.convert_any` — wrap an internal reference as an `externref`. Null stays null.
#[inline]
#[must_use]
pub fn externalize(v: Value) -> Value {
    if v == NULL_REF { v } else { v | EXTERN_TAG }
}

/// `any.convert_extern` — unwrap an `externref` back to the internal reference it carries.
/// Null stays null.
///
/// ⚠️ An `externref` that never went through [`externalize`] — one the *host* handed in — has no
/// wrapper bit to clear, and unwrapping it must still yield a host reference rather than a bare
/// integer that the `Any` arm would read as a GC index. That is why the boundary tags with
/// `HOST_TAG | EXTERN_TAG` and this function only ever clears `EXTERN_TAG`.
#[inline]
#[must_use]
pub fn internalize(v: Value) -> Value {
    if v == NULL_REF { v } else { v & !EXTERN_TAG }
}

// --- funcref encoding: (owning instance, function index) ------------------------------
//
// **A `funcref` carries the identity of the instance that produced it.** Without that it is a bare
// function index, and `call_indirect` resolves an index against the *calling* instance — so the moment
// two instances share a table, instance A's `ref.func $a` is called as B's function of the same index.
// A silent wrong call, which is why imported tables were refused until this existed (T9a#4).
//
// Layout, and the two constraints that fix it:
//
// ```text
//   bit 63      bits 62..32           bits 31..0
//   0           instance index        function index
//   ^ MUST be 0
// ```
//
// * **Bit 63 must stay clear**, because it is [`I31_TAG`]. The obvious layout — instance in bits
//   32..=63 — collides with it, and a colliding funcref would read as an i31.
// * `NULL_REF` is *all* 64 bits set, so it can never be a packed funcref for the same reason.
//
// The property that makes this safe to introduce: **instance 0 packs to the bare index**, so every
// single-instance program keeps bit-identical values and only genuinely cross-instance references
// change. That is why landing this moved the conformance suite not at all.
const FUNCREF_INSTANCE_SHIFT: u32 = 32;
/// Instances addressable by a `funcref`. 31 bits, not 32 — bit 63 belongs to [`I31_TAG`].
const MAX_INSTANCES: usize = 1 << 31;

/// Pack an owning instance and a function index into a `funcref` value.
#[inline]
fn pack_funcref(instance: usize, func: u32) -> Value {
    ((instance as Value) << FUNCREF_INSTANCE_SHIFT) | Value::from(func)
}

/// The instance that produced a `funcref`. Callers must have established it is not `NULL_REF`.
#[inline]
fn funcref_instance(v: Value) -> usize {
    ((v >> FUNCREF_INSTANCE_SHIFT) & 0x7fff_ffff) as usize
}

/// The function index within its owning instance.
#[inline]
fn funcref_index(v: Value) -> u32 {
    v as u32
}

/// Default cap on live GC objects per instance. There is no collector (a proposal-scope
/// decision), so this backstop keeps a guest allocation loop from exhausting host memory.
const DEFAULT_MAX_GC_OBJECTS: usize = 1 << 24;

/// Default cap on live boxed exceptions per instance. `catch_ref`/`catch_all_ref` box an
/// exception so it can become an `exnref` value; like the GC heap there is no collector, so
/// this backstop keeps a guest catch loop from exhausting host memory.
const DEFAULT_MAX_EXN_BOXES: usize = 1 << 20;

/// The resource ceilings a [`Store`] enforces on the guests it runs.
///
/// These were compile-time constants until T8. They are per-store configuration now because
/// a C embedder has no other way to reach them — and because two of them are load-bearing
/// for an embedder specifically:
///
/// - **`max_call_depth`** — the interpreter recurses on the *host* stack, so a debug build
///   can overflow it before the 512-frame cap fires (`cmem/known-issues.md`). An embedder
///   linking the debug `cdylib` is exposed to that; lowering this is the fix available to
///   them without diverging the shipped default from the oracle.
/// - **`max_memory_bytes`** — a guest that genuinely needs more than 1 GiB could not run at
///   all before this was reachable.
///
/// Every field is a **ceiling, not a reservation**: raising one costs nothing until a guest
/// actually asks for the memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Total linear-memory bytes an instance may hold, summed across its memories.
    pub max_memory_bytes: usize,
    /// Total table entries an instance may hold, summed across its tables.
    pub max_table_elems: usize,
    /// Maximum guest call depth before [`Trap::CallStackExhausted`].
    pub max_call_depth: usize,
    /// Maximum live GC objects before [`Trap::GcHeapExhausted`].
    pub max_gc_objects: usize,
    /// Maximum live boxed exceptions before [`Trap::ExnStoreExhausted`].
    pub max_exn_boxes: usize,
}

impl ResourceLimits {
    /// The shipped defaults — identical to the pre-T8 compile-time constants, so a store
    /// built without configuration behaves exactly as it always has.
    #[must_use]
    pub const fn defaults() -> ResourceLimits {
        ResourceLimits {
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_table_elems: DEFAULT_MAX_TABLE_ELEMS,
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            max_gc_objects: DEFAULT_MAX_GC_OBJECTS,
            max_exn_boxes: DEFAULT_MAX_EXN_BOXES,
        }
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        ResourceLimits::defaults()
    }
}

/// A heap-allocated GC object: its declared type index (RTT) + its struct fields / array
/// elements. One `Value` (128-bit) per field — enough for every field type incl. `v128`.
struct HeapObject {
    /// The instance that ALLOCATED this object. `type_index` is numbered in *that* instance's
    /// module, so without this an object crossing a link is read against the wrong type table.
    ///
    /// ⚠️ The same rule a funcref already encodes by packing its owner into bits 62..32
    /// (see `ref_matches`). `gc_heap` lives on `Pools`, shared by every instance in a linking group,
    /// so objects genuinely cross modules and a bare index decided nothing.
    owner: u32,
    type_index: u32,
    fields: Vec<Value>,
}

// `owner` is FREE: it lands in the padding `Vec`'s 8-byte alignment already forced after a bare
// `u32`. Pinned so that a future change making it cost real memory fails the build instead of
// quietly growing every GC object — the same guard `Instr`'s `offset` carries.
const _: () = assert!(core::mem::size_of::<HeapObject>() == core::mem::size_of::<Vec<Value>>() + 8);

/// A thrown exception in flight: the tag it carries and its value payload. Owned (a `Vec`
/// per exception) rather than arena-allocated — it frees when the last holder drops.
#[derive(Clone, PartialEq, Eq)]
struct Exception {
    tag: u32,
    values: Vec<Value>,
}

/// One inline handler of a legacy `try`. `tag == None` is `catch_all`; `handler_pc` is the
/// first instruction after the `catch`.
#[derive(Clone, Copy)]
struct LegacyCatch {
    tag: Option<u32>,
    handler_pc: usize,
}

/// The catch handlers (and optional `delegate` label) of a legacy `try`, precomputed per
/// `try` instruction so unwinding can find them without rescanning the body.
#[derive(Clone, Default)]
struct LegacyTry {
    handlers: Vec<LegacyCatch>,
    delegate: Option<u32>,
}

/// Shared linear memory. `bytes` is `alloc_zeroed`-backed (demand-zero pages on mainstream
/// OSes), so a large declared minimum costs address space, not resident memory.
pub struct Memory {
    pub bytes: Vec<u8>,
    pub max: Option<u64>,
    pub is64: bool,
    /// A `shared` memory (threads proposal) — required by `memory.atomic.wait*`.
    pub shared: bool,
}

/// A reference table: `Value` slots (`NULL_REF` = uninitialized; a funcref is its function
/// index) so funcref and externref tables share one representation.
pub struct Table {
    pub entries: Vec<Value>,
    pub max: Option<u32>,
    /// The element type, for import matching (§4.5.9). The matching **minimum** is the table's
    /// *current* length, not a stored declared value — see [`table_import_matches`].
    pub element: crate::types::ValType,
    /// The table's **index type**: `i64` (table64) or `i32`. Recorded for import matching, where
    /// it must be EQUAL, not merely compatible — it decides what type every `table.get`/`set`/
    /// `grow`/`fill` operand has, so neither direction substitutes for the other.
    ///
    /// ⚠️ [`Memory`] carried this from the start and `Table` did not, so a 64-bit table satisfied
    /// a 32-bit import and vice versa — four `assert_unlinkable`s in `memory64-imports.wast`.
    pub is64: bool,
}

/// The mutable runtime state of an instance, threaded as `&mut` through execution so a
/// recursive `call` reborrows it cleanly.
/// Where one instance's index spaces live inside the shared [`Store`] pools.
///
/// This is the shared-store model (as wasmtime does it): resources are owned **once** by the
/// store, and each instance keeps a map from its own index space to the store slot. A
/// *defined* memory allocates a fresh slot; an *imported* one maps to the exporting
/// instance's existing slot, so both instances genuinely see the same bytes.
///
/// Chosen over the alternatives for concrete reasons: the oracle's `*Memory` raw pointer is
/// exactly what the safety directive forbids, and `Rc<RefCell<Memory>>` would put a borrow
/// check on the interpreter's hottest path. A `Vec` index costs one indirection and no
/// runtime check.
#[derive(Default, Clone)]
struct IndexMaps {
    memories: Vec<usize>,
    tables: Vec<usize>,
    globals: Vec<usize>,
    /// Store slots holding this instance's data-segment "dropped" flags.
    data: Vec<usize>,
    /// Store slots holding this instance's element segments.
    elems: Vec<usize>,
}

impl IndexMaps {
    /// Resolve a module-level index to its store slot. Out-of-range yields `usize::MAX`,
    /// which every pool lookup then rejects as the corresponding "no such resource" trap —
    /// so a bad index can never silently alias another instance's resource.
    #[inline]
    fn get(map: &[usize], i: usize) -> usize {
        map.get(i).copied().unwrap_or(usize::MAX)
    }
    #[inline]
    fn mem(&self, i: u32) -> usize {
        Self::get(&self.memories, i as usize)
    }
    #[inline]
    fn table(&self, i: u32) -> usize {
        Self::get(&self.tables, i as usize)
    }
    #[inline]
    fn global(&self, i: u32) -> usize {
        Self::get(&self.globals, i as usize)
    }
    #[inline]
    fn data(&self, i: u32) -> usize {
        Self::get(&self.data, i as usize)
    }
    #[inline]
    fn elem(&self, i: u32) -> usize {
        Self::get(&self.elems, i as usize)
    }
}

/// The resource pools shared by every instance in a linking group.
#[derive(Default)]
struct Pools {
    /// The ceilings this store enforces. Lives here rather than on [`Store`] because the
    /// runtime threads `&mut Pools` down every execution path, so every site that has to
    /// consult a limit already holds it.
    limits: ResourceLimits,
    globals: Vec<Value>,
    memories: Vec<Memory>,
    tables: Vec<Table>,
    /// `data_dropped[i]` marks a data segment consumed — active segments are dropped once
    /// instantiation copies them in (§4.5.4); `data.drop` marks a passive one.
    data_dropped: Vec<bool>,
    /// Evaluated reference values of each element segment (for `table.init`).
    elem_values: Vec<Vec<Value>>,
    /// `elem_dropped[i]` marks a segment consumed (active/declarative at init, or `elem.drop`).
    elem_dropped: Vec<bool>,
    /// GC heap: one entry per allocated struct/array; a GC reference value is its index here.
    /// No collector — objects live for the instance's lifetime, bounded by `MAX_GC_OBJECTS`.
    ///
    /// 🔒 **ONE heap per store (per linking group), NOT one per instance — do not "consolidate" this
    /// onto `InstanceData`.** A GC reference is a bare index into this vector, so if each instance
    /// owned a heap, the same index would select a *different object* on the other side of a link:
    /// object substitution, silently, with no type error. Keeping the heap store-wide makes an index
    /// mean one thing everywhere, which is why that whole defect class cannot occur here.
    ///
    /// ⚠️ It is *this* invariant that makes [`HeapObject::owner`] necessary rather than redundant:
    /// one shared heap means objects genuinely cross module boundaries, so each one must carry the
    /// instance whose type table numbers its `type_index`. The two decisions are a pair — a shared
    /// heap without the owner tag is the cross-module type-confusion bug fixed 2026-08-14.
    gc_heap: Vec<HeapObject>,
    /// Exceptions boxed so they can be `exnref` values (`catch_ref`/`catch_all_ref` push one,
    /// `throw_ref` consumes one) — an `exnref` value is an index here. Only the `_ref` forms
    /// box; an ordinary `throw`/`catch` round-trip never touches this, so a throwing loop
    /// does not grow it. Bounded by `MAX_EXN_BOXES`.
    exn_store: Vec<Exception>,
    /// An exception unwinding across call frames: set when it leaves a frame uncaught, so the
    /// caller's `call` site can try to catch it. Cleared once caught or once it escapes the
    /// invocation.
    pending_exn: Option<Exception>,
    /// Store-wide type identity, shared by every instance in this store.
    ///
    /// Lives on `Pools` rather than [`Store`] because the interpreter needs it: a `call_indirect` on
    /// a shared table must decide whether the callee's type satisfies the declared one, and the two
    /// now come from different modules. `Pools` is what execution already threads.
    ///
    /// Placed **last** deliberately. It is consulted only at link time and on a *cross-instance*
    /// `call_indirect`, so it must not sit among the fields the interpreter touches per instruction —
    /// putting it after `limits` measured ~7% slower on the steady-state loop, which touches no types
    /// at all. Field order in a hot struct is not cosmetic.
    types: TypeRegistry,
    /// The call stack of the trap that is currently unwinding, innermost frame first.
    ///
    /// Built **on the way out**, one entry per frame as the error passes through it, rather than
    /// maintained as a shadow stack during execution: a shadow stack costs a push and a pop on every
    /// call whether or not anything ever traps, and calls are hot. This costs nothing until something
    /// actually goes wrong. Also placed last, for the reason above.
    backtrace: Vec<TrapFrame>,
}

/// One frame of a trap's call stack — what [`Store::backtrace`] hands back and what the C ABI's
/// `wasmrt_trap_frame` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapFrame {
    /// Which instance in this store the frame was running in.
    pub instance: usize,
    /// Index in the instance's **function index space** (imports included), so it lines up with
    /// the name section and with `ref.func`.
    pub func_index: u32,
    /// Byte offset of the trapping instruction **from the start of the module** — the form
    /// `wasm-objdump` prints and every wasm tool resolves, rather than a body-relative offset the
    /// consumer would have to add a base to. `Code::body_offset` exists to make this cheap.
    pub offset: u32,
}

/// A runtime trap or an execution-setup error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    Unreachable,
    DivByZero,
    IntOverflow,
    /// A trapping float→int conversion of NaN, infinity, or an out-of-range value.
    InvalidConversionToInt,
    CallStackExhausted,
    UndefinedExport,
    BadArgCount,
    UndefinedFunc,
    UndefinedGlobal,
    UndefinedLabel,
    StackUnderflow,
    UnbalancedControl,
    /// A memory instruction in a module with no linear memory (or a bad memory index).
    NoMemory,
    /// A memory access (or data-segment init) outside the memory bounds.
    MemoryOutOfBounds,
    /// The declared/grown linear memory exceeds this instance's budget.
    MemoryLimitExceeded,
    /// An atomic access whose effective address is not naturally aligned to its width.
    UnalignedAtomic,
    /// `memory.atomic.wait*` on a non-shared memory.
    ExpectedSharedMemory,
    /// A `memory.init` / `data.drop` data-segment index out of range.
    UndefinedData,
    /// `call_indirect` in a module with no table (or a bad table index).
    NoTable,
    /// A table access (or element-segment init) outside the table bounds.
    TableOutOfBounds,
    /// The declared/grown table exceeds this instance's budget.
    TableLimitExceeded,
    /// `call_indirect` hit an uninitialized (null) table element.
    UninitializedElement,
    /// `call_indirect`'s declared type did not match the callee's signature.
    IndirectTypeMismatch,
    /// A type index out of range.
    UndefinedType,
    /// A `table.init` / `elem.drop` element-segment index out of range.
    UndefinedElement,
    /// A null reference where a non-null one is required (`call_ref` / `ref.as_non_null`).
    NullReference,
    /// A GC struct field / array element access outside the object's bounds.
    GcOutOfBounds,
    /// The instance allocated more than `MAX_GC_OBJECTS` GC objects (no collector).
    GcHeapExhausted,
    /// `ref.cast` to a type the value is not an instance of.
    CastFailure,
    /// A `throw` / catch tag index out of range (EH).
    UndefinedTag,
    /// A thrown exception reached the top of the invocation with no matching handler (EH).
    /// Not a bug in the guest's sense — it is how an escaping wasm exception surfaces.
    UncaughtException,
    /// More than `MAX_EXN_BOXES` exceptions were boxed as `exnref` values (no collector).
    ExnStoreExhausted,
    /// A constant expression used an opcode this slice doesn't evaluate.
    ConstantExpr,
    /// An opcode this interpreter slice does not execute yet (float/memory/tables/GC/SIMD/EH).
    UnsupportedInstruction,
    /// The host supplied fewer (or more) imports than the module declares.
    MissingImport,
    /// An imported **table**. A table holds `funcref`s, and a `funcref` is currently a bare
    /// function index with no instance identity — so a table shared between two instances
    /// would have `call_indirect` resolve an entry against the *calling* instance and dispatch
    /// to the wrong function. Refused loudly until the funcref encoding carries its owner
    /// (`cmem/known-issues.md`, T9a#4). Imported *memories* are supported: bytes have no
    /// identity problem.
    UnsupportedImportKind,
    /// An import resolved to a definition of the right kind whose **type** does not satisfy the
    /// declared import type (§4.5.9) — a memory whose limits are too narrow, or a function whose
    /// signature differs from the one the importer declared.
    IncompatibleImport,
    /// A host function signalled failure. Hosts may also return any other [`Trap`].
    HostTrap,
    /// A body failed to decode at instantiation.
    Decode(DecodeError),
}

impl From<DecodeError> for Trap {
    fn from(e: DecodeError) -> Self {
        Trap::Decode(e)
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trap::Unreachable => f.write_str("unreachable executed"),
            Trap::DivByZero => f.write_str("integer divide by zero"),
            Trap::IntOverflow => f.write_str("integer overflow"),
            Trap::InvalidConversionToInt => f.write_str("invalid conversion to integer"),
            Trap::CallStackExhausted => f.write_str("call stack exhausted"),
            Trap::UndefinedExport => f.write_str("no such exported function"),
            Trap::BadArgCount => f.write_str("wrong number of arguments"),
            Trap::UndefinedFunc => f.write_str("function index out of range"),
            Trap::UndefinedGlobal => f.write_str("global index out of range"),
            Trap::UndefinedLabel => f.write_str("branch label out of range"),
            Trap::StackUnderflow => f.write_str("operand stack underflow"),
            Trap::UnbalancedControl => f.write_str("unbalanced control instruction"),
            Trap::UndefinedTag => f.write_str("tag index out of range"),
            Trap::UncaughtException => f.write_str("uncaught exception"),
            Trap::ExnStoreExhausted => f.write_str("too many live exception references"),
            Trap::NoMemory => f.write_str("no linear memory"),
            Trap::MemoryOutOfBounds => f.write_str("out of bounds memory access"),
            Trap::MemoryLimitExceeded => f.write_str("memory exceeds the instance budget"),
            Trap::UnalignedAtomic => f.write_str("unaligned atomic access"),
            Trap::ExpectedSharedMemory => f.write_str("atomic wait on non-shared memory"),
            Trap::UndefinedData => f.write_str("data segment index out of range"),
            Trap::NoTable => f.write_str("no table"),
            Trap::TableOutOfBounds => f.write_str("out of bounds table access"),
            Trap::TableLimitExceeded => f.write_str("table exceeds the instance budget"),
            Trap::UninitializedElement => f.write_str("uninitialized table element"),
            Trap::IndirectTypeMismatch => f.write_str("indirect call type mismatch"),
            Trap::UndefinedType => f.write_str("type index out of range"),
            Trap::UndefinedElement => f.write_str("element segment index out of range"),
            Trap::NullReference => f.write_str("null reference"),
            Trap::GcOutOfBounds => f.write_str("out of bounds GC access"),
            Trap::GcHeapExhausted => f.write_str("GC heap exhausted"),
            Trap::CastFailure => f.write_str("cast failure"),
            Trap::ConstantExpr => f.write_str("unsupported constant expression"),
            Trap::UnsupportedInstruction => {
                f.write_str("instruction not executed in this release (float/memory/tables/GC/SIMD/EH)")
            }
            Trap::MissingImport => f.write_str("import count does not match the module"),
            Trap::UnsupportedImportKind => {
                f.write_str("imported tables are not linkable yet")
            }
            Trap::IncompatibleImport => {
                f.write_str("an import does not match the declared import type")
            }
            Trap::HostTrap => f.write_str("host function trapped"),
            Trap::Decode(e) => write!(f, "decode error at instantiation: {e}"),
        }
    }
}

impl core::error::Error for Trap {}

/// A trap or setup result.
pub type Result<T> = core::result::Result<T, Trap>;

/// A defined function prepared for execution (compute slice: one slot per local/value).
struct FuncBody {
    ty: FuncType,
    /// params + declared locals (one `u64` slot each — no v128 in this slice).
    num_locals: usize,
    ir: Vec<Instr>,
    /// For each `block`/`loop`/`if`/`else` index: the matching `end` index.
    end_of: Vec<usize>,
    /// For each `if` index: the `else` index, or `ir.len()` if none.
    else_of: Vec<usize>,
    /// For each legacy `try` index: its inline catch handlers + optional `delegate` label;
    /// `None` elsewhere (EH, legacy encoding).
    try_info: Vec<Option<LegacyTry>>,
    /// Absolute module offset of this body's first instruction, so a trap frame can report a
    /// module offset rather than a body-relative one the consumer would have to rebase.
    body_offset: u32,
}

/// What a host function can reach while it runs: the calling instance's linear memories, so
/// it can read arguments out of guest memory and write results back (how every WASI call
/// works). Handed to the callback for the duration of the call and no longer.
pub struct Caller<'a> {
    store: &'a mut Pools,
    /// The CALLING instance's index maps: a host function's `memory(0)` means that
    /// instance's memory 0, wherever it lives in the shared pools.
    maps: &'a IndexMaps,
}

impl Caller<'_> {
    /// Byte length of memory `index`, or `None` if the instance has no such memory.
    #[must_use]
    pub fn memory_len(&self, index: u32) -> Option<usize> {
        self.store.memories.get(self.maps.mem(index)).map(|m| m.bytes.len())
    }

    /// Read `len` bytes of guest memory at `addr`. `None` if the range is out of bounds —
    /// a host function must treat that as the guest's error, not panic on it.
    #[must_use]
    pub fn read(&self, index: u32, addr: u64, len: usize) -> Option<&[u8]> {
        let m = self.store.memories.get(self.maps.mem(index))?;
        let start = usize::try_from(addr).ok()?;
        let end = start.checked_add(len)?;
        m.bytes.get(start..end)
    }

    /// Mutable view of `len` bytes of guest memory at `addr`, bounds-checked as [`read`].
    #[must_use]
    pub fn write(&mut self, index: u32, addr: u64, len: usize) -> Option<&mut [u8]> {
        let m = self.store.memories.get_mut(self.maps.mem(index))?;
        let start = usize::try_from(addr).ok()?;
        let end = start.checked_add(len)?;
        m.bytes.get_mut(start..end)
    }
}

/// The backing for one imported function.
///
/// A boxed closure rather than a raw function pointer plus a `void*` context: the context
/// pointer is what a Zig-style host-callback ABI would use, and it cannot be expressed
/// without `unsafe`. A closure carries its state safely and costs one indirection at a call
/// boundary that is already the slow path (see the safety directive in
/// `cmem/design-decisions.md`).
pub struct HostFunc {
    call: alloc::boxed::Box<HostFnBody>,
}

/// The callback shape behind a [`HostFunc`]: arguments in, a results slice sized to the
/// import's declared arity out, `Err` to trap the guest.
type HostFnBody = dyn Fn(&mut Caller<'_>, &[Value], &mut [Value]) -> Result<()>;

impl HostFunc {
    /// Wrap a host callback. It receives the call's arguments and a results slice already
    /// sized to the import's declared result arity; returning `Err` traps the guest.
    pub fn new(
        f: impl Fn(&mut Caller<'_>, &[Value], &mut [Value]) -> Result<()> + 'static,
    ) -> HostFunc {
        HostFunc {
            call: alloc::boxed::Box::new(f),
        }
    }
}

impl fmt::Debug for HostFunc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostFunc(..)")
    }
}

/// Host-supplied backing for a module's imports, in **per-kind import order**: each vector
/// aligns with the module's imports of that kind, which occupy the low indices of their
/// index space.
#[derive(Default)]
pub struct Imports {
    /// Function backings **in declaration order**, host and wasm interleaved as the module
    /// declares them. Keeping one ordered vector rather than a vector per kind is what lets
    /// a module mix `(import "spectest" "print")` with `(import "a" "f")` and still bind
    /// each to the right slot.
    funcs: Vec<ImportedFunc>,
    pub globals: Vec<Value>,
    /// Memory backings **in declaration order**. A memory is never copied in: the backing
    /// names another instance's memory, and instantiation resolves it to that instance's
    /// store slot, so both instances index the same bytes.
    memories: Vec<MemoryImport>,
    /// Table backings **in declaration order**, each naming another instance's table. Like a memory,
    /// never copied — and correct only because a `funcref` now carries its owning instance.
    tables: Vec<TableImport>,
}

impl fmt::Debug for Imports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The backings are closures with no useful representation; the counts are what a
        // caller diagnosing a link actually wants.
        f.debug_struct("Imports")
            .field("funcs", &self.funcs.len())
            .field("globals", &self.globals.len())
            .field("memories", &self.memories.len())
            .field("tables", &self.tables.len())
            .finish()
    }
}

/// One function import's backing.
///
/// The wasm case keeps the whole [`InstanceId`], **store tag included**, rather than the bare index
/// [`FuncTarget`] runs on: instantiation is where the tag is checked and the id lowered to a slot, so
/// a backing naming another store's instance is refused instead of quietly becoming a valid-looking
/// index into this one.
enum ImportedFunc {
    Host(HostFunc),
    Wasm { instance: InstanceId, func: u32 },
}

/// One memory import's backing: **another instance's** memory, named by that instance and its
/// own memory index. Deliberately not a store slot — an embedder holds [`InstanceId`]s and has
/// no business knowing pool layout, and resolving here keeps the mapping in one place (and is
/// where the issuing store is verified).
#[derive(Clone, Copy)]
struct MemoryImport {
    instance: InstanceId,
    index: u32,
}

/// One table import's backing: another instance's table, named by that instance and its own table
/// index. Refused outright until a `funcref` carried its owner (T9a#4).
#[derive(Clone, Copy)]
struct TableImport {
    instance: InstanceId,
    index: u32,
}

impl Imports {
    #[must_use]
    pub fn new() -> Imports {
        Imports::default()
    }

    /// Append a host function, in the order the module declares its function imports.
    #[must_use]
    pub fn with_host_func(mut self, f: HostFunc) -> Imports {
        self.funcs.push(ImportedFunc::Host(f));
        self
    }

    /// Append a host function from a closure, in the order the module declares its
    /// function imports.
    #[must_use]
    pub fn with_func(
        mut self,
        f: impl Fn(&mut Caller<'_>, &[Value], &mut [Value]) -> Result<()> + 'static,
    ) -> Imports {
        self.funcs.push(ImportedFunc::Host(HostFunc::new(f)));
        self
    }

    /// Append an imported global's value, in declaration order.
    #[must_use]
    pub fn with_global(mut self, v: Value) -> Imports {
        self.globals.push(v);
        self
    }

    /// Satisfy a function import with **another instance's** function (module linking).
    /// The callee runs against its own instance, so it sees the exporter's memory and
    /// globals — which is what makes `(register …)` linking behave correctly.
    #[must_use]
    pub fn with_instance_func(mut self, instance: InstanceId, func: u32) -> Imports {
        self.funcs.push(ImportedFunc::Wasm { instance, func });
        self
    }

    /// Satisfy a memory import with **another instance's** memory, named by that instance's
    /// own memory index. Appended in the order the module declares its memory imports.
    ///
    /// The two instances then share the *same* bytes — a write through either is visible
    /// through the other by construction, because both maps hold one store slot. That is the
    /// whole point of an imported memory, and it is why nothing is copied.
    #[must_use]
    pub fn with_instance_memory(mut self, instance: InstanceId, index: u32) -> Imports {
        self.memories.push(MemoryImport { instance, index });
        self
    }

    /// Satisfy a table import with **another instance's** table, named by that instance's own table
    /// index. Appended in the order the module declares its table imports.
    ///
    /// Both instances then index the *same* entries. This is only correct because a `funcref` carries
    /// the instance that produced it: `call_indirect` resolves an entry against its **owner**, so a
    /// reference the exporter stored still reaches the exporter's function when the importer calls it.
    /// Before that encoding existed this was refused outright rather than dispatched wrongly.
    #[must_use]
    pub fn with_instance_table(mut self, instance: InstanceId, index: u32) -> Imports {
        self.tables.push(TableImport { instance, index });
        self
    }
}

/// Does an existing table satisfy a declared import type (§4.5.9)?
///
/// Limits like a memory's — the actual minimum at least the declared one, a declared maximum requiring
/// an actual no larger. The **element type must be equal**, not merely a subtype: a table is mutable,
/// so a narrower actual element type would let the importer write a value the exporter's type forbids.
fn table_import_matches(actual: &Table, declared: &crate::module::TableType) -> bool {
    // ⚠️ The matching minimum is the table's **CURRENT length**, not the minimum it was declared
    // with. A table *instance*'s type has `min = |elem|`, and `table.grow` updates it — so a table
    // declared `(table 0 funcref)` and grown to 2 *does* satisfy an `(import … (table 2 funcref))`.
    // `table_grow.wast` states it in a comment: "imported table limits should match, because
    // external table size is 2 now."
    //
    // wasmrt got this wrong for **memories** first, storing the declared minimum and asserting that
    // in a test; no memory case in the suite contradicted it, and the table case did. Both now read
    // the current size, which equals the declared minimum until something grows.
    actual.is64 == declared.limits.is64
        && actual.element == declared.element
        && actual.entries.len() as u64 >= declared.limits.min
        && match declared.limits.max {
            Some(m) => actual.max.is_some_and(|a| u64::from(a) <= m),
            None => true,
        }
}

/// Do two signatures from **different modules** denote the same type?
///
/// Each side's concrete references are resolved through its *own* module's store-wide type ids, so the
/// comparison is between store-wide identities rather than module-local indices. That is the exactness
/// the type registry buys: without it, this question had no correct answer available at all.
///
/// Still equality rather than subtyping. §4.5.9 matching is subtyping, and deciding it across modules
/// needs the supertype chains resolved store-wide too — a further step, not done here, and logged. The
/// direction of the residual approximation is unchanged: equality can only **refuse** a link that subtyping
/// would allow, never accept one it would refuse.
fn cross_module_func_types_match(
    a_ids: &[u32],
    a: &crate::module::FuncType,
    b_ids: &[u32],
    b: &crate::module::FuncType,
) -> bool {
    a.params.len() == b.params.len()
        && a.results.len() == b.results.len()
        && a.params
            .iter()
            .zip(&b.params)
            .all(|(&x, &y)| cross_module_val_types_match(a_ids, x, b_ids, y))
        && a.results
            .iter()
            .zip(&b.results)
            .all(|(&x, &y)| cross_module_val_types_match(a_ids, x, b_ids, y))
}

/// One value type, compared across modules. Non-reference and abstract-reference types compare by
/// their bits (they carry no module-local index); a **concrete** reference compares by nullability plus
/// the store-wide id of its target.
fn cross_module_val_types_match(a_ids: &[u32], a: crate::types::ValType, b_ids: &[u32], b: crate::types::ValType) -> bool {
    if !a.is_concrete() || !b.is_concrete() {
        return a == b;
    }
    if a.is_non_null_ref() != b.is_non_null_ref() {
        return false;
    }
    // Both ids must EXIST and match. An absent id means the module named a type it does not have — an
    // invalid module the validator refuses; treating absence as a match would let two such modules link.
    match (
        a_ids.get(a.concrete_index() as usize),
        b_ids.get(b.concrete_index() as usize),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Does an existing memory satisfy a declared import type? §4.5.9 matching, which compares
/// **types**: the actual minimum must be at least the declared one, and a declared maximum
/// requires the actual to have one no larger (an unbounded memory never satisfies a bounded
/// import). The index type and `shared` flag must be equal, not merely compatible — they change
/// what instructions are legal against the memory, so neither direction is substitutable.
fn memory_import_matches(actual: &Memory, declared: &crate::module::MemoryType) -> bool {
    // The matching minimum is the memory's **CURRENT size in pages** — a memory instance's type has
    // `min = |bytes| / 64Ki` and `memory.grow` updates it. This originally stored and compared the
    // *declared* minimum, which no memory case in the suite contradicted; the equivalent table case
    // did (`table_grow.wast`), and the rule is the same for both.
    actual.is64 == declared.limits.is64
        && actual.shared == declared.limits.shared
        && (actual.bytes.len() / PAGE_SIZE) as u64 >= declared.limits.min
        && match declared.limits.max {
            Some(m) => actual.max.is_some_and(|a| a <= m),
            None => true,
        }
}

/// What backs one of an instance's **imported** functions.
#[derive(Clone, Copy)]
enum FuncTarget {
    /// A host callback, at this index in the store's host-function pool.
    Host(usize),
    /// Another instance's function (module linking) — it runs against **its own** instance,
    /// so its memory and globals are the exporter's, not the importer's.
    Wasm { instance: usize, func: u32 },
}

/// One instance's immutable code and index maps.
///
/// Deliberately separate from [`Pools`]: a cross-instance call needs `&code[callee]` while
/// still holding `&mut pools`, and two disjoint fields of [`Store`] borrow cleanly where two
/// halves of one struct would not. That is what makes wasm→wasm calls work without `Rc`,
/// `RefCell`, or `unsafe`.
struct InstanceData {
    module: Module,
    /// Store-wide type id of each of this module's types, from the store's [`TypeRegistry`]. Two
    /// instances' types are the same type iff these ids match — which is what makes cross-module
    /// import matching an integer comparison rather than a structural walk.
    type_ids: Vec<u32>,
    func_bodies: Vec<FuncBody>,
    maps: IndexMaps,
    /// One entry per **imported** function, in declaration order.
    imports: Vec<FuncTarget>,
}

/// A group of instances that share resources — the unit of linking.
///
/// Modelled on wasmtime: the store owns every memory, table and global exactly once, and
/// instances reference them by index. Two instances that share a memory hold the same slot
/// in their maps, so a write through one is visible through the other by construction.
pub struct Store {
    /// This store's identity, stamped into every [`InstanceId`] it issues.
    id: u64,
    code: Vec<InstanceData>,
    host_funcs: Vec<HostFunc>,
    pools: Pools,
}

impl Default for Store {
    fn default() -> Store {
        Store {
            // Relaxed is enough: the value is only ever compared for equality, and no other memory
            // ordering depends on it.
            id: NEXT_STORE_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            code: Vec::new(),
            host_funcs: Vec::new(),
            pools: Pools::default(),
        }
    }
}

/// **Store-wide type identity** — the registry that makes types comparable across modules.
///
/// `Module::type_canon` decides identity *within* a module: it is the lowest local index structurally
/// equal to a type. Two modules number their types independently, so those ids say nothing across a
/// boundary, and the structural *key* cannot be compared either because it embeds them by reference.
/// Making keys self-contained (inlining each referenced group) would work but blows up exponentially on
/// chained groups — a denial-of-service surface on exactly the untrusted path.
///
/// So groups are **interned** as each module joins the store. A group's key references outside targets
/// by their already-assigned **store-wide** id, which is available because groups are interned in index
/// order and an outside reference is always to an earlier group. Interning is content-addressed, so two
/// modules spelling out the same group land on the same id and cross-module matching becomes an integer
/// comparison at link time — never on a hot path.
///
/// This is wasmtime's *shape*, written here from the architecture rather than its code (the standing
/// rule in `cmem/design-decisions.md`).
#[derive(Default)]
struct TypeRegistry {
    /// Structural key of each distinct rec group → the store-wide id of its **first** member.
    /// A `BTreeMap` for the same reason `canonicalize` uses one: group counts are attacker-controlled,
    /// and a linear scan would be O(groups²).
    groups: alloc::collections::BTreeMap<Vec<u8>, u32>,
    /// Declared supertype of each store-wide type id, as a store-wide id. Recorded because import
    /// matching is **subtyping**, not equality (§4.5.9), so the chain has to be walkable store-wide.
    supers: Vec<Option<u32>>,
    /// Next unused store-wide type id. Ids are allocated in blocks of a group's length, so a group's
    /// members occupy consecutive ids and member *position* is preserved.
    next: u32,
}

impl TypeRegistry {
    /// The store-wide id of the first member of the group with this key, interning it if new.
    ///
    /// `member_supers` gives each member's declared supertype as a store-wide id, recorded only on
    /// first intern — a repeat is the same group by definition, so its chain is already there.
    fn intern(&mut self, key: Vec<u8>, len: u32, member_supers: &[Option<u32>]) -> u32 {
        if let Some(&base) = self.groups.get(&key) {
            return base;
        }
        let base = self.next;
        // Saturating rather than wrapping: on overflow every later group collapses onto one id, which
        // would make unrelated types compare equal. Saturating keeps them merely un-interned, and a
        // store holding 2^32 distinct rec groups has other problems.
        self.next = self.next.saturating_add(len.max(1));
        self.groups.insert(key, base);
        let end = base as usize + len as usize;
        if self.supers.len() < end {
            self.supers.resize(end, None);
        }
        for (i, s) in member_supers.iter().enumerate() {
            if let Some(slot) = self.supers.get_mut(base as usize + i) {
                *slot = *s;
            }
        }
        base
    }

    /// Is store-wide type `sub` a subtype of `sup`, by the declared supertype chain?
    ///
    /// Terminates because a supertype is always a lower id: within a group it is an earlier member,
    /// and outside it is a group interned earlier, whose block starts lower.
    fn is_subtype(&self, sub: u32, sup: u32) -> bool {
        let mut cur = Some(sub);
        while let Some(c) = cur {
            if c == sup {
                return true;
            }
            cur = self.supers.get(c as usize).copied().flatten();
        }
        false
    }

    /// Assign store-wide ids to every type in `module`, in group order.
    ///
    /// Returns one id per type index. A module with no recorded group extents (one built by hand rather
    /// than decoded) is treated as all singletons, which is what the spec says an ungrouped type is.
    fn assign(&mut self, module: &Module) -> Vec<u32> {
        let singletons: Vec<(u32, u32)> = (0..module.comp_types.len() as u32)
            .map(|i| (i, 1))
            .collect();
        let groups = if module.rec_groups.is_empty() {
            &singletons
        } else {
            &module.rec_groups
        };
        let mut ids: Vec<u32> = Vec::with_capacity(module.comp_types.len());
        for &(start, len) in groups {
            // `ids` is filled in index order, so it holds exactly the earlier groups' store-wide ids —
            // which is what `rec_group_key_with` needs for references pointing out of this group.
            let key = crate::module::rec_group_key_with(
                &module.comp_types,
                &module.supertypes,
                &module.type_finals,
                &ids,
                start,
                len,
            );
            // Each member's declared supertype as a store-wide id: an earlier member of this group
            // resolves against the block being allocated, anything else is already assigned.
            let base_guess = self.groups.get(&key).copied().unwrap_or(self.next);
            let member_supers: Vec<Option<u32>> = (0..len)
                .map(|i| {
                    let s = module.supertypes.get((start + i) as usize).copied().flatten()?;
                    if s >= start && s < start + len {
                        Some(base_guess + (s - start))
                    } else {
                        ids.get(s as usize).copied()
                    }
                })
                .collect();
            let base = self.intern(key, len, &member_supers);
            for i in 0..len {
                ids.push(base + i);
            }
        }
        ids
    }
}

/// Monotonic source of store identities. Only ever compared for equality, and starts at 1 so a
/// zero-initialized [`InstanceId`] can never name a real store — the same reason the C ABI's value
/// handles pack a `+1`.
static NEXT_STORE_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

/// Identifies an instance inside a [`Store`] — **and identifies which store issued it**.
///
/// The store tag is not bookkeeping. Without it, an id obtained from store X and passed to store Y
/// indexes *Y's* instance vector, and if the index happens to be in range the caller silently gets an
/// unrelated instance's memory or function: measured, before this existed, as a guest linking against
/// a foreign store's memory and reading its own instead. It also removes a panic — several accessors
/// index `code[id]` directly, so an out-of-range foreign id aborted the process under
/// `panic = "abort"`.
///
/// This is the same defence the C ABI already applies to its value handles (each carries the identity
/// of the store that issued it, so a foreign or stale one is refused rather than followed). Core had
/// the weaker guarantee of the two; now it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceId {
    store: u64,
    index: usize,
}

/// An instantiated module ready to run — a [`Store`] holding exactly one instance.
///
/// The convenience shape for the common single-module case; use [`Store`] directly when
/// modules need to import from one another.
pub struct Instance {
    store: Store,
    id: InstanceId,
}

/// Immutable execution context (read-only during a call); the mutable [`Store`] is threaded
/// separately as `&mut` so a recursive `call` reborrows it cleanly.
struct Ctx<'a> {
    module: &'a Module,
    /// Where this instance's index spaces live in the shared store pools.
    maps: &'a IndexMaps,
    /// Every instance's code, so a `call` into an imported wasm function can reach the
    /// callee's instance.
    code: &'a [InstanceData],
    host_funcs: &'a [HostFunc],
    /// Which instance this frame belongs to.
    inst: usize,
}

impl Instance {
    /// The call stack of the most recent trap, innermost first — see [`Store::backtrace`].
    #[must_use]
    pub fn backtrace(&self) -> &[TrapFrame] {
        self.store.backtrace()
    }

    /// The function name for a frame, from the name section — see [`Store::frame_name`].
    #[must_use]
    pub fn frame_name(&self, frame: &TrapFrame) -> Option<&[u8]> {
        self.store.frame_name(frame)
    }

    /// Instantiate a decoded module with no imports.
    ///
    /// # Validation is the CALLER's responsibility — and it is not optional
    ///
    /// This takes a decoded [`Module`], not a *validated* one, and **does not validate it**.
    /// §4.5.1 defines instantiation only for valid modules, so running an unvalidated one is
    /// outside the spec: the interpreter's invariants (stack heights, operand types, index
    /// ranges) are exactly what validation establishes. wasmrt is `forbid(unsafe_code)`, so the
    /// worst case is a wrong answer or a panic rather than memory unsafety — but a wrong answer
    /// is the failure mode this project ranks as its worst.
    ///
    /// **Call [`crate::validate::validate`] first.** The split is deliberate — it matches
    /// wasmtime's compile/instantiate separation and keeps validation off the instantiation path
    /// for callers who have already done it (the C ABI validates in `wasmrt_module_new`; the CLI
    /// validates before every execute path). It is a precondition, not a suggestion.
    ///
    /// # Errors
    /// Returns [`Trap::MissingImport`] if the module declares any import — use
    /// [`Instance::new_with_imports`] to supply them.
    pub fn new(module: Module) -> Result<Instance> {
        Instance::new_with_imports(module, Imports::default())
    }

    /// Instantiate a decoded module, linking `imports` against its declared imports.
    ///
    /// The vectors are matched **per kind, in declaration order** — imports occupy the low
    /// indices of their index space, so `imports.funcs[0]` backs function 0.
    ///
    /// # Errors
    /// [`Trap::MissingImport`] if a count does not match what the module declares, or
    /// [`Trap::UnsupportedImportKind`] for an imported table. A memory import needs another
    /// instance to share from, so it is only reachable through [`Store`] — a lone `Instance`
    /// declaring one gets [`Trap::MissingImport`].
    pub fn new_with_imports(module: Module, imports: Imports) -> Result<Instance> {
        Instance::new_with(module, imports, ResourceLimits::defaults())
    }

    /// Instantiate a decoded module with `imports`, under explicit resource ceilings.
    ///
    /// # Errors
    /// As [`Instance::new_with_imports`], plus [`Trap::MemoryLimitExceeded`] /
    /// [`Trap::TableLimitExceeded`] if the module's declared minimums exceed `limits`.
    pub fn new_with(
        module: Module,
        imports: Imports,
        limits: ResourceLimits,
    ) -> Result<Instance> {
        let mut store = Store::with_limits(limits);
        let id = store.instantiate(module, imports)?;
        Ok(Instance { store, id })
    }

    /// The wrapped module.
    ///
    /// Infallible here, unlike [`Store::module_of`]: an `Instance` owns its store and its own id, so
    /// the two cannot belong to different stores by construction.
    #[must_use]
    pub fn module(&self) -> &Module {
        self.store
            .module_of(self.id)
            .expect("an Instance always holds an id its own store issued")
    }

    /// Invoke an exported function by name.
    ///
    /// # Errors
    /// [`Trap::UndefinedExport`] if there is no such export, or whatever the guest traps
    /// with.
    pub fn invoke(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        self.store.invoke(self.id, name, args)
    }

    /// Invoke a function by its index in the function index space.
    ///
    /// # Errors
    /// [`Trap::UndefinedFunc`] for a bad index, [`Trap::BadArgCount`] on arity mismatch, or
    /// whatever the guest traps with.
    pub fn invoke_index(&mut self, func_index: u32, args: &[Value]) -> Result<Vec<Value>> {
        self.store.invoke_index(self.id, func_index, args)
    }
}

impl Store {
    /// A store with no instances, using the default [`ResourceLimits`].
    #[must_use]
    pub fn new() -> Store {
        Store::default()
    }

    /// A store with no instances, enforcing `limits` on every guest it runs.
    #[must_use]
    pub fn with_limits(limits: ResourceLimits) -> Store {
        let mut s = Store::default();
        s.pools.limits = limits;
        s
    }

    /// The ceilings this store enforces.
    #[must_use]
    pub fn limits(&self) -> ResourceLimits {
        self.pools.limits
    }

    /// Resolve an [`InstanceId`] to a slot in **this** store, or `None` if the id was issued by a
    /// different store or names an instance this one does not have.
    ///
    /// Every accessor goes through here. The alternative — each one indexing `code[id.index]` — is
    /// what let a foreign id silently reach an unrelated instance, and panicked when its index was
    /// out of range.
    #[inline]
    fn slot(&self, id: InstanceId) -> Option<usize> {
        (id.store == self.id && id.index < self.code.len()).then_some(id.index)
    }

    /// The module behind an instance, or `None` if the id belongs to another store.
    #[must_use]
    pub fn module_of(&self, id: InstanceId) -> Option<&Module> {
        Some(&self.code[self.slot(id)?].module)
    }

    /// Find an exported function's index in an instance, for linking against it.
    #[must_use]
    pub fn export_func(&self, id: InstanceId, name: &str) -> Option<u32> {
        self.code.get(self.slot(id)?)?.module.exports.iter().find_map(|e| {
            (e.name == name && e.ty.kind() == crate::types::ExternKind::Func).then_some(e.index)
        })
    }

    /// Read an instance's exported global, for linking another module against it.
    ///
    /// **A snapshot, not a live binding.** An imported global is passed to the importer
    /// *by value* ([`Imports::with_global`]), so a later `global.set` in the exporter is not
    /// observed by the importer. That is exact for immutable globals — which is what the
    /// spec permits importing in the first place, absent the mutable-globals extension —
    /// and it is why this returns `Value` rather than a handle.
    #[must_use]
    pub fn export_global(&self, id: InstanceId, name: &str) -> Option<Value> {
        let data = self.code.get(self.slot(id)?)?;
        let index = data.module.exports.iter().find_map(|e| {
            (e.name == name && e.ty.kind() == crate::types::ExternKind::Global).then_some(e.index)
        })?;
        self.pools.globals.get(data.maps.global(index)).copied()
    }

    /// The declared **type** of an instance's exported global.
    ///
    /// Separate from [`Store::export_global`], which returns the value, because import matching
    /// (§4.5.9) compares types: a value alone cannot say whether the global was declared
    /// mutable, and `i32`/`f32` are indistinguishable once both are bits in a slot.
    #[must_use]
    pub fn export_global_type(
        &self,
        id: InstanceId,
        name: &str,
    ) -> Option<crate::module::GlobalType> {
        let data = self.code.get(self.slot(id)?)?;
        let index = data.module.exports.iter().find_map(|e| {
            (e.name == name && e.ty.kind() == crate::types::ExternKind::Global).then_some(e.index)
        })?;
        data.module.globals.get(index as usize).copied()
    }

    /// How many instances this store holds. An [`InstanceId`] is valid here iff its index
    /// is below this.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.code.len()
    }

    /// The index (in the instance's own index space) of an export of the given kind.
    #[must_use]
    pub fn export_index(
        &self,
        id: InstanceId,
        name: &str,
        kind: crate::types::ExternKind,
    ) -> Option<u32> {
        self.code.get(self.slot(id)?)?.module.exports.iter().find_map(|e| {
            (e.name == name && e.ty.kind() == kind).then_some(e.index)
        })
    }

    /// Shared access to one of an instance's memories, by the instance's own memory index.
    ///
    /// Routed through the instance's [`IndexMaps`], so an *imported* memory resolves to the
    /// exporter's storage — the caller sees the same bytes the guest does.
    #[must_use]
    pub fn memory(&self, id: InstanceId, index: u32) -> Option<&Memory> {
        let data = self.code.get(self.slot(id)?)?;
        self.pools.memories.get(data.maps.mem(index))
    }

    /// Mutable access to one of an instance's memories. See [`Store::memory`].
    #[must_use]
    pub fn memory_mut(&mut self, id: InstanceId, index: u32) -> Option<&mut Memory> {
        let slot = self.code.get(self.slot(id)?)?.maps.mem(index);
        self.pools.memories.get_mut(slot)
    }

    /// Read one of an instance's globals by the instance's own global index. A snapshot —
    /// see [`Store::export_global`].
    #[must_use]
    pub fn global(&self, id: InstanceId, index: u32) -> Option<Value> {
        let data = self.code.get(self.slot(id)?)?;
        self.pools.globals.get(data.maps.global(index)).copied()
    }

    /// The signature of a function in an instance's function index space.
    #[must_use]
    pub fn func_type(&self, id: InstanceId, func_index: u32) -> Option<crate::module::FuncType> {
        self.code.get(self.slot(id)?)?.module.func_type(func_index)
    }

    /// Whether an instance exports `name` with the given kind — the link-time existence
    /// check, without reading the value.
    #[must_use]
    pub fn has_export(&self, id: InstanceId, name: &str, kind: crate::types::ExternKind) -> bool {
        self.slot(id).and_then(|s| self.code.get(s)).is_some_and(|d| {
            d.module
                .exports
                .iter()
                .any(|e| e.name == name && e.ty.kind() == kind)
        })
    }

    /// Instantiate a module into this store, linking `imports` against its declared imports.
    ///
    /// # Errors
    /// [`Trap::MissingImport`] if a count does not match, [`Trap::UnsupportedImportKind`] for
    /// an imported table, [`Trap::IncompatibleImport`] if a supplied memory does not satisfy
    /// the declared import type, or a trap from evaluating an initializer or applying an active
    /// segment.
    pub fn instantiate(&mut self, module: Module, imports: Imports) -> Result<InstanceId> {
        if module.functions.len() != module.code.len() {
            return Err(Trap::UndefinedFunc);
        }

        // Count the declared imports per kind. Each kind's imports occupy the low indices of
        // its space, so these are also the offsets between an index and its defined slot.
        let (mut n_funcs, mut n_globals, mut n_mems, mut n_tables) = (0usize, 0, 0, 0);
        for imp in &module.imports {
            match imp.ty.kind() {
                crate::types::ExternKind::Func => n_funcs += 1,
                crate::types::ExternKind::Global => n_globals += 1,
                crate::types::ExternKind::Memory => n_mems += 1,
                crate::types::ExternKind::Table => n_tables += 1,
                // A tag import needs no host backing — its type is all the engine uses.
                crate::types::ExternKind::Tag => {}
            }
        }
        // Imported tables are LINKABLE as of T9a#4's second half. They were refused until a `funcref`
        // carried its owning instance, because a shared table would otherwise have had
        // `call_indirect` resolve an entry against the *calling* instance — a silent wrong call.
        // A function import may be backed by a host callback OR another instance's export,
        // so both pools count toward the declared total.
        if imports.funcs.len() != n_funcs
            || imports.globals.len() != n_globals
            || imports.memories.len() != n_mems
            || imports.tables.len() != n_tables
        {
            return Err(Trap::MissingImport);
        }

        // Intern this module's rec groups, giving every one of its types a **store-wide** id. Done
        // here — after the cheap count checks, before the type checks that need it — because the
        // import matching below compares types across a module boundary, which module-local canonical
        // ids cannot do.
        //
        // A later failure in this function leaves the interned groups behind. That is benign in a way
        // orphaned pool slots were not: interning is content-addressed, so nothing references them and
        // an identical group later reuses the same ids. It does mean repeated *failed* instantiations of
        // distinct type sections grow the registry, which is noted in `known-issues.md`.
        let type_ids = self.pools.types.assign(&module);

        // Import type matching for functions (§4.5.9). Checked here rather than in the linker
        // because this is the one place every caller passes through — a hand-built `Imports`
        // gets the same check the `Linker` does.
        //
        // Only a backing whose type is KNOWN can be checked. A [`HostFunc`] is a bare closure
        // with no declared signature (the C ABI cannot express one either), so a host import is
        // taken on trust; a wasm→wasm backing does carry a signature, and binding it to a
        // mismatched declaration is the silent-wrong-call class — the guest would marshal
        // arguments for one shape and the callee read another.
        let declared_funcs = module.imports.iter().filter_map(|i| match &i.ty {
            crate::module::Extern::Func(ft) => Some((ft, i.func_type_index)),
            _ => None,
        });
        for (decl, backing) in declared_funcs.zip(&imports.funcs) {
            let (declared, declared_ti) = decl;
            if let ImportedFunc::Wasm { instance, func } = backing {
                let exporter = self.slot(*instance).ok_or(Trap::MissingImport)?;
                let actual = self.code[exporter]
                    .module
                    .func_type(*func)
                    .ok_or(Trap::MissingImport)?;
                // Matched by **store-wide type IDENTITY, with subtyping** — not by comparing the two
                // signatures' shapes.
                //
                // Shape comparison cannot answer this question at all. `M10` exports a `(func)` whose
                // declared type sits in a rec group whose sibling refers *outward*; the importer
                // declares a `(func)` from a group whose sibling refers *inward*. Both signatures are
                // the empty `(func)`, so any param/result comparison links them — and the spec says
                // they are different types and must not link. Rec-group membership is part of
                // identity, and only the type *index* carries it.
                //
                // And §4.5.9 matching is **subtyping**, not equality: `M` exporting `f1: $t1` links
                // against a declared `$t0` when `$t1 <: $t0`. Equality refused three valid modules.
                let matched = match (
                    declared_ti.and_then(|ti| type_ids.get(ti as usize).copied()),
                    self.code[exporter]
                        .module
                        .func_type_index(*func)
                        .and_then(|ti| self.code[exporter].type_ids.get(ti as usize).copied()),
                ) {
                    (Some(want), Some(got)) => self.pools.types.is_subtype(got, want),
                    // No index on one side — a re-exported *import* has no defining type index, and a
                    // hand-built `Module` has no registry ids. Fall back to the structural comparison
                    // rather than refusing: it is what this check did before, and it is right whenever
                    // no concrete reference is involved. Logged as the residual.
                    _ => cross_module_func_types_match(
                        &type_ids,
                        declared,
                        &self.code[exporter].type_ids,
                        &actual,
                    ),
                };
                if !matched {
                    return Err(Trap::IncompatibleImport);
                }
            }
        }

        // Globals: the imported values occupy the low indices, then each defined
        // initializer is evaluated against everything already in scope (so a defined global
        // may read an imported one, which is exactly what the spec allows).
        // The index this instance will occupy. Known before it is pushed, and needed *now* because
        // a `ref.func` in any initializer must be stamped with its owning instance.
        let self_inst = self.code.len();
        if self_inst >= MAX_INSTANCES {
            // A funcref addresses its instance in 31 bits (bit 63 is `I31_TAG`). Refuse loudly at the
            // ceiling rather than silently truncating an instance index into someone else's.
            return Err(Trap::TableLimitExceeded);
        }

        let mut globals: Vec<Value> = imports.globals;
        globals.reserve(module.global_inits.len());
        for init in &module.global_inits {
            let v = eval_const_expr(init, &globals, self_inst, Some((&module, &mut self.pools)))?;
            globals.push(v);
        }

        // Imported memories occupy the LOW memory indices, so resolve them first — each to the
        // exporting instance's existing store slot, never to a fresh allocation. `maps.mem`
        // yields `usize::MAX` for an out-of-range index, which the pool lookup then rejects, so
        // a bogus backing cannot alias some other instance's memory.
        let mut imported_mems: Vec<usize> = Vec::with_capacity(n_mems);
        for (i, im) in imports.memories.iter().enumerate() {
            // `slot()` is what refuses an id from another store. Before it existed, a foreign id's
            // index resolved against THIS store and the guest silently shared the wrong memory.
            let slot = self
                .slot(im.instance)
                .map_or(usize::MAX, |s| self.code[s].maps.mem(im.index));
            let actual = self.pools.memories.get(slot).ok_or(Trap::MissingImport)?;
            // `module.memories` is the whole index space, imports first, so index `i` is this
            // import's declared type.
            let declared = module.memories.get(i).ok_or(Trap::MissingImport)?;
            if !memory_import_matches(actual, declared) {
                return Err(Trap::IncompatibleImport);
            }
            imported_mems.push(slot);
        }

        // Linear memories: allocate each *defined* memory sized to its declared minimum
        // (demand-zero via `vec![0; n]`), bounded by the per-instance budget.
        let defined_mems = module.memories.get(n_mems..).unwrap_or(&[]);
        let mut memories: Vec<Memory> = Vec::with_capacity(defined_mems.len());
        let mut total_bytes: usize = 0;
        for mt in defined_mems {
            let min_pages = usize::try_from(mt.limits.min).map_err(|_| Trap::MemoryLimitExceeded)?;
            let nbytes = min_pages
                .checked_mul(PAGE_SIZE)
                .ok_or(Trap::MemoryLimitExceeded)?;
            total_bytes = total_bytes
                .checked_add(nbytes)
                .filter(|&t| t <= self.pools.limits.max_memory_bytes)
                .ok_or(Trap::MemoryLimitExceeded)?;
            memories.push(Memory {
                bytes: vec![0u8; nbytes],
                max: mt.limits.max,
                is64: mt.limits.is64,
                shared: mt.limits.shared,
            });
        }

        // Apply active data segments, then mark them (and only them) dropped (§4.5.4).
        //
        // A segment may target an imported memory, which already lives in the pools, or a
        // defined one, which is still local until this instantiation commits. Splitting on the
        // index keeps the *defined* resources out of the store until every step has succeeded —
        // so a later failure leaves no orphaned slots. An imported memory is inherently shared,
        // so a write to it is visible whatever happens next; that is the exporter's memory, and
        // the spec's instantiation is not transactional over it either.
        for seg in &module.data {
            if !seg.active {
                continue;
            }
            let mi = seg.mem_index as usize;
            let mem: &mut Memory = if mi < n_mems {
                let slot = *imported_mems.get(mi).ok_or(Trap::NoMemory)?;
                self.pools.memories.get_mut(slot).ok_or(Trap::NoMemory)?
            } else {
                memories.get_mut(mi - n_mems).ok_or(Trap::NoMemory)?
            };
            let offset = eval_const_offset(&seg.offset_expr, &globals, mem.is64)?;
            let start = usize::try_from(offset).map_err(|_| Trap::MemoryOutOfBounds)?;
            let end = start
                .checked_add(seg.bytes.len())
                .filter(|&e| e <= mem.bytes.len())
                .ok_or(Trap::MemoryOutOfBounds)?;
            mem.bytes[start..end].copy_from_slice(&seg.bytes);
        }
        let data_dropped: Vec<bool> = module.data.iter().map(|s| s.active).collect();

        // Imported tables occupy the LOW table indices — resolved, like memories, to the exporting
        // instance's existing store slot. Correct only because a `funcref` now carries its owning
        // instance (T9a#4): without that, `call_indirect` on a shared table would resolve an entry
        // against the *calling* instance and silently call the wrong function.
        let mut imported_tables: Vec<usize> = Vec::with_capacity(n_tables);
        for (i, it) in imports.tables.iter().enumerate() {
            let slot = self
                .slot(it.instance)
                .map_or(usize::MAX, |s| self.code[s].maps.table(it.index));
            let actual = self.pools.tables.get(slot).ok_or(Trap::MissingImport)?;
            let declared = module.tables.get(i).ok_or(Trap::MissingImport)?;
            if !table_import_matches(actual, declared) {
                return Err(Trap::IncompatibleImport);
            }
            imported_tables.push(slot);
        }

        // Tables: allocate each *defined* table sized to its minimum, filled with `NULL_REF`,
        // bounded by the per-instance entry budget.
        let defined_tables = module.tables.get(n_tables..).unwrap_or(&[]);
        let mut tables: Vec<Table> = Vec::with_capacity(defined_tables.len());
        let mut total_elems: usize = 0;
        for tt in defined_tables {
            let min = usize::try_from(tt.limits.min).map_err(|_| Trap::TableLimitExceeded)?;
            total_elems = total_elems
                .checked_add(min)
                .filter(|&t| t <= self.pools.limits.max_table_elems)
                .ok_or(Trap::TableLimitExceeded)?;
            // A table declared with an initializer expression (function-references) starts
            // with every entry set to it, not null. Evaluated against the globals resolved
            // so far, exactly as a global's own initializer is.
            let fill = match &tt.init {
                Some(expr) => {
                    eval_const_expr(expr, &globals, self_inst, Some((&module, &mut self.pools)))?
                }
                None => NULL_REF,
            };
            tables.push(Table {
                entries: vec![fill; min],
                max: tt.limits.max.and_then(|m| u32::try_from(m).ok()),
                element: tt.element,
                is64: tt.limits.is64,
            });
        }

        // Evaluate element segments to reference values; apply the active ones to their table
        // (then drop them and the declarative ones; passive stay for `table.init`).
        let mut elem_values: Vec<Vec<Value>> = Vec::with_capacity(module.elements.len());
        let mut elem_dropped: Vec<bool> = Vec::with_capacity(module.elements.len());
        for elem in &module.elements {
            let mut vals: Vec<Value> = Vec::with_capacity(elem.funcs.len() + elem.exprs.len());
            // The funcidx shorthand (elem forms 0-3) denotes `ref.func` of THIS instance, so the
            // values are stamped exactly as the instruction form would stamp them.
            vals.extend(elem.funcs.iter().map(|&f| pack_funcref(self_inst, f)));
            for ex in &elem.exprs {
                vals.push(eval_const_expr(
                    ex,
                    &globals,
                    self_inst,
                    Some((&module, &mut self.pools)),
                )?);
            }
            if elem.mode == crate::module::ElementMode::Active {
                // Forks on the index exactly as data segments do: an *imported* table already lives
                // in the pools, a *defined* one is still local until this instantiation commits, so
                // defined resources stay out of the store until every step has succeeded.
                let ti = elem.table_index as usize;
                let tbl: &mut Table = if ti < n_tables {
                    let slot = *imported_tables.get(ti).ok_or(Trap::NoTable)?;
                    self.pools.tables.get_mut(slot).ok_or(Trap::NoTable)?
                } else {
                    tables.get_mut(ti - n_tables).ok_or(Trap::NoTable)?
                };
                let offset = eval_const_offset(&elem.offset_expr, &globals, false)?; // tables are 32-bit
                let start = usize::try_from(offset).map_err(|_| Trap::TableOutOfBounds)?;
                let end = start
                    .checked_add(vals.len())
                    .filter(|&e| e <= tbl.entries.len())
                    .ok_or(Trap::TableOutOfBounds)?;
                tbl.entries[start..end].copy_from_slice(&vals);
            }
            elem_dropped.push(elem.mode != crate::module::ElementMode::Passive);
            elem_values.push(vals);
        }

        // Prepare each defined function: decode its body + precompute control flow.
        let mut func_bodies = Vec::with_capacity(module.functions.len());
        for (&type_index, code) in module.functions.iter().zip(&module.code) {
            let ty = module.func_sig(type_index).ok_or(Trap::UndefinedFunc)?;
            let num_locals = ty.params.len() + code.local_count() as usize;
            // Cloned, not re-decoded: the module carries the decoded body already (a clone of the
            // instruction vector is cheaper than walking the bytes again, and this ran once per
            // instantiation).
            let ir = code.ir.clone();
            let cf = precompute_control_flow(&ir)?;
            func_bodies.push(FuncBody {
                ty,
                num_locals,
                ir,
                end_of: cf.end_of,
                else_of: cf.else_of,
                try_info: cf.try_info,
                body_offset: code.body_offset,
            });
        }

        // Move this instance's resources into the shared pools and record where they
        // landed. A DEFINED resource takes a fresh slot; the maps are what let an imported
        // one point at another instance's existing slot instead — everything downstream
        // already reads through them.
        let base = |n: usize, len: usize| (n..n + len).collect::<Vec<_>>();
        let mem_base = self.pools.memories.len();
        let maps = IndexMaps {
            // Imported memories keep the exporter's slots; defined ones take fresh slots after
            // them. Index order therefore matches the module's own memory index space.
            memories: imported_mems
                .iter()
                .copied()
                .chain(mem_base..mem_base + memories.len())
                .collect(),
            // Imported tables keep the exporter's slots; defined ones take fresh slots after them,
            // so index order matches the module's own table index space.
            tables: {
                let tbl_base = self.pools.tables.len();
                imported_tables
                    .iter()
                    .copied()
                    .chain(tbl_base..tbl_base + tables.len())
                    .collect()
            },
            globals: base(self.pools.globals.len(), globals.len()),
            data: base(self.pools.data_dropped.len(), data_dropped.len()),
            elems: base(self.pools.elem_values.len(), elem_values.len()),
        };
        self.pools.memories.extend(memories);
        self.pools.tables.extend(tables);
        self.pools.globals.extend(globals);
        self.pools.data_dropped.extend(data_dropped);
        self.pools.elem_values.extend(elem_values);
        self.pools.elem_dropped.extend(elem_dropped);

        // Walk the backings IN DECLARATION ORDER so each import binds to its own slot;
        // host callbacks join the store-wide pool as they are seen.
        let mut targets = Vec::with_capacity(imports.funcs.len());
        for f in imports.funcs {
            targets.push(match f {
                ImportedFunc::Host(h) => {
                    self.host_funcs.push(h);
                    FuncTarget::Host(self.host_funcs.len() - 1)
                }
                // This is where a store-tagged id becomes a bare slot for the hot path — so it is
                // also the last point at which a foreign one can be refused. The type check above
                // already resolved it once; resolving again here keeps `FuncTarget` free of the tag
                // rather than paying for it on every cross-instance call.
                ImportedFunc::Wasm { instance, func } => FuncTarget::Wasm {
                    instance: self.slot(instance).ok_or(Trap::MissingImport)?,
                    func,
                },
            });
        }

        let id = InstanceId {
            store: self.id,
            index: self.code.len(),
        };
        let start = module.start;
        self.code.push(InstanceData {
            module,
            type_ids,
            func_bodies,
            maps,
            imports: targets,
        });

        // §4.5.5 step 11: the start function runs as the LAST step of instantiation, after every
        // element and data segment is in place — it is allowed to observe and modify them.
        //
        // A trap here fails the instantiation, and the half-built instance stays in `self.code`.
        // That matches the spec's "the module is not instantiated" only in what the caller gets
        // back: it never receives the `InstanceId`, so it can neither call into the instance nor
        // name it as an import. The slot itself is not reclaimed, for the same reason the pool
        // slots above are not — index stability is what makes every other `InstanceId` in this
        // store keep meaning what it meant.
        if let Some(func_index) = start {
            self.invoke_index(id, func_index, &[])?;
        }
        Ok(id)
    }

    /// Invoke an instance's exported function by name.
    ///
    /// # Errors
    /// [`Trap::UndefinedExport`] if there is no such export, or whatever the guest traps
    /// with.
    pub fn invoke(&mut self, id: InstanceId, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        let func_index = self.export_func(id, name).ok_or(Trap::UndefinedExport)?;
        self.invoke_index(id, func_index, args)
    }

    /// Invoke a function by its index in an instance's function index space.
    ///
    /// # Errors
    /// [`Trap::UndefinedFunc`] for a bad index **or an id issued by another store**,
    /// [`Trap::BadArgCount`] on arity mismatch, or whatever the guest traps with.
    pub fn invoke_index(
        &mut self,
        id: InstanceId,
        func_index: u32,
        args: &[Value],
    ) -> Result<Vec<Value>> {
        // Resolved before anything else: without the store check this indexed `code` directly, so a
        // foreign id either called an unrelated instance's function or panicked.
        let inst = self.slot(id).ok_or(Trap::UndefinedFunc)?;
        let ft = self.code[inst]
            .module
            .func_type(func_index)
            .ok_or(Trap::UndefinedFunc)?;
        if args.len() != ft.params.len() {
            return Err(Trap::BadArgCount);
        }
        // The EH state is per-invocation: an exception that escaped a previous call must not
        // be visible to this one, and the exnrefs it boxed are unreachable once it returns.
        self.pools.pending_exn = None;
        self.pools.exn_store.clear();
        // Likewise per-invocation: a stale backtrace read after a *successful* call would describe
        // the previous failure, which is worse than describing nothing.
        self.pools.backtrace.clear();
        // Borrow the code immutably and the pools mutably — disjoint fields, which is what
        // lets a nested cross-instance call take another `&code[…]` without conflict.
        let Store {
            code,
            host_funcs,
            pools,
            ..
        } = self;
        let r = call_function(code, host_funcs, pools, inst, func_index, args, 1);
        // Drop an escaping exception's payload rather than pinning it until the next invoke.
        if r.is_err() {
            pools.pending_exn = None;
        }
        r
    }

    /// The call stack of the most recent trap, **innermost frame first**.
    ///
    /// Empty after a successful call, and empty for a failure that never entered wasm (a bad
    /// argument count, an unknown export): those have no wasm frames to report, and inventing one
    /// would be worse than saying nothing.
    ///
    /// Valid until the next [`Store::invoke_index`]; a trap object that must outlive that should
    /// copy what it needs.
    #[must_use]
    pub fn backtrace(&self) -> &[TrapFrame] {
        &self.pools.backtrace
    }

    /// Resolve a frame's function to its name from the module's name section. `None` when the
    /// module carries no name for it — a stripped module gets the index and nothing else, which is
    /// honest. Raw bytes, not `str`: the name section is only required to be UTF-8 by convention,
    /// and this must not fail on a module that breaks that.
    #[must_use]
    pub fn frame_name(&self, frame: &TrapFrame) -> Option<&[u8]> {
        self.code
            .get(frame.instance)?
            .module
            .func_name(frame.func_index)
    }
}

/// The per-body control-flow tables computed once at instantiation (see
/// [`precompute_control_flow`]), each indexed by instruction pc.
struct ControlFlow {
    end_of: Vec<usize>,
    else_of: Vec<usize>,
    try_info: Vec<Option<LegacyTry>>,
}

/// Match every `block`/`loop`/`if`/`try`/`try_table` with its `end`, every `if` with its
/// `else`, and collect each legacy `try`'s inline `catch`/`catch_all` handlers.
///
/// The handler collection rides the same opener stack: when a `catch` is reached, the top
/// opener is always its enclosing `try` (the body's nested blocks are balanced before the
/// first `catch`). At the `try`'s `end` we also point `end_of` at that `end` for every one of
/// its `catch` instructions, so a body — or a handler — that completes normally skips the
/// remaining handlers instead of falling into them.
fn precompute_control_flow(ir: &[Instr]) -> Result<ControlFlow> {
    let mut end_of = vec![0usize; ir.len()];
    let mut else_of = vec![ir.len(); ir.len()]; // sentinel = "no else"
    let mut try_info: Vec<Option<LegacyTry>> = vec![None; ir.len()];
    let mut stack: Vec<usize> = Vec::new();
    // Parallel to `stack`: the handlers collected so far, and the pcs of their `catch`
    // instructions (so `end_of` can be back-filled at the `end`).
    let mut handlers: Vec<Vec<LegacyCatch>> = Vec::new();
    let mut catch_pcs: Vec<Vec<usize>> = Vec::new();
    for (i, instr) in ir.iter().enumerate() {
        match instr.op {
            Op::Block | Op::Loop | Op::If | Op::TryTable | Op::TryLegacy => {
                stack.push(i);
                handlers.push(Vec::new());
                catch_pcs.push(Vec::new());
            }
            Op::Else => {
                let &opener = stack.last().ok_or(Trap::UnbalancedControl)?;
                else_of[opener] = i;
            }
            Op::CatchLegacy | Op::CatchAll => {
                let top = handlers.last_mut().ok_or(Trap::UnbalancedControl)?; // bare catch
                top.push(LegacyCatch {
                    tag: if instr.op == Op::CatchLegacy {
                        Some(tag_imm(instr)?)
                    } else {
                        None
                    },
                    handler_pc: i + 1,
                });
                catch_pcs
                    .last_mut()
                    .ok_or(Trap::UnbalancedControl)?
                    .push(i);
            }
            Op::Delegate => {
                // `delegate` terminates its `try` in place of an `end`.
                let opener = stack.pop().ok_or(Trap::UnbalancedControl)?; // bare delegate
                handlers.pop();
                catch_pcs.pop();
                end_of[opener] = i;
                try_info[opener] = Some(LegacyTry {
                    handlers: Vec::new(),
                    delegate: Some(label_imm(instr)?),
                });
            }
            Op::End => {
                if let Some(opener) = stack.pop() {
                    let hs = handlers.pop().unwrap_or_default();
                    let cps = catch_pcs.pop().unwrap_or_default();
                    end_of[opener] = i;
                    if else_of[opener] != ir.len() {
                        end_of[else_of[opener]] = i;
                    }
                    if ir[opener].op == Op::TryLegacy {
                        for cp in cps {
                            end_of[cp] = i; // a catch reached by normal flow skips to the end
                        }
                        try_info[opener] = Some(LegacyTry {
                            handlers: hs,
                            delegate: None,
                        });
                    }
                }
                // else: the function's implicit final `end`.
            }
            _ => {}
        }
    }
    Ok(ControlFlow {
        end_of,
        else_of,
        try_info,
    })
}

/// A control label on a frame's label stack.
///
/// The EH fields identify the construct by its **pc** rather than borrowing its immediate:
/// the catch clauses live in `body.ir[try_pc]` and the legacy handlers in
/// `body.try_info[try_pc]`, both immutable for the frame's lifetime, so the label stays cheap
/// to push and free of a second borrow of the body.
#[derive(Clone)]
struct Label {
    is_loop: bool,
    /// Slots carried on a branch (results for block/if, params for loop).
    arity: u32,
    /// pc to jump to on a branch to this label.
    target: usize,
    /// Value-stack height below this construct's operands.
    stack_base: usize,
    /// `Some(pc)` only for a `try_table` label — its catch clauses are `body.ir[pc]`'s,
    /// consulted when an exception unwinds through this frame.
    try_table_pc: Option<usize>,
    /// `Some(pc)` only for a legacy `try` label — its handlers are `body.try_info[pc]`'s.
    legacy_pc: Option<usize>,
    /// The exception currently being handled in this legacy try's catch block, kept so
    /// `rethrow` can re-raise it. Set when a handler is entered.
    caught: Option<Exception>,
}

/// A plain (non-EH) label — the common case for `block`/`loop`/`if`.
fn plain_label(is_loop: bool, arity: u32, target: usize, stack_base: usize) -> Label {
    Label {
        is_loop,
        arity,
        target,
        stack_base,
        try_table_pc: None,
        legacy_pc: None,
        caught: None,
    }
}

struct Frame<'a> {
    body: &'a FuncBody,
    locals: Vec<Value>,
    vstack: Vec<Value>,
    labels: Vec<Label>,
}

impl Frame<'_> {
    fn push(&mut self, v: Value) {
        self.vstack.push(v);
    }
    /// Pop one slot; a short stack (only reachable from an unvalidated body) yields a defined
    /// 0 and is turned into a trap by `run` before the next instruction consumes it unsafely.
    fn pop(&mut self) -> Value {
        self.vstack.pop().unwrap_or(0)
    }
    fn push_i32(&mut self, v: i32) {
        self.push(i32_value(v));
    }
    fn pop_i32(&mut self) -> i32 {
        as_i32(self.pop())
    }
    fn push_i64(&mut self, v: i64) {
        self.push(i64_value(v));
    }
    fn pop_i64(&mut self) -> i64 {
        as_i64(self.pop())
    }
    fn push_f32(&mut self, v: f32) {
        self.push(f32_value(v));
    }
    fn pop_f32(&mut self) -> f32 {
        as_f32(self.pop())
    }
    fn push_f64(&mut self, v: f64) {
        self.push(f64_value(v));
    }
    fn pop_f64(&mut self) -> f64 {
        as_f64(self.pop())
    }
    /// Pop a memory address/count: i64 for a memory64 memory, else i32 (zero-extended).
    fn pop_mem(&mut self, is64: bool) -> u64 {
        if is64 {
            self.pop_i64() as u64
        } else {
            u64::from(self.pop_i32() as u32)
        }
    }

    fn stack_base(&self, n: usize) -> Result<usize> {
        self.vstack.len().checked_sub(n).ok_or(Trap::StackUnderflow)
    }

    fn branch(&mut self, n: u32) -> Result<usize> {
        if n as usize >= self.labels.len() {
            return Err(Trap::UndefinedLabel);
        }
        // Read the scalars out first — `Label` owns a `caught` exception, so it is not `Copy`.
        let (is_loop, arity, target, label_base) = {
            let l = &self.labels[self.labels.len() - 1 - n as usize];
            (l.is_loop, l.arity as usize, l.target, l.stack_base)
        };
        let from = self.stack_base(arity)?;
        if from < label_base {
            return Err(Trap::StackUnderflow);
        }
        self.vstack.copy_within(from..from + arity, label_base);
        self.vstack.truncate(label_base + arity);
        // A loop-continue keeps the loop's own label; a forward exit pops it too.
        let keep = if is_loop {
            self.labels.len() - n as usize
        } else {
            self.labels.len() - (n as usize + 1)
        };
        self.labels.truncate(keep);
        Ok(target)
    }

    /// Try to catch `exn` in this frame: search the label stack innermost-out for a handler
    /// that matches. Returns the resumption pc on a match, or `None` if nothing in this frame
    /// handles it (the caller then propagates [`Trap::UncaughtException`]).
    ///
    /// The two encodings unwind differently, and the difference is load-bearing:
    /// - a `try_table` clause branches **out of** the try_table to the clause's label;
    /// - a legacy `catch` runs **inside** the try, whose label stays on the stack so
    ///   `rethrow` can still name it.
    fn throw_exception(&mut self, store: &mut Pools, exn: &Exception) -> Result<Option<usize>> {
        let body = self.body;
        for d in 0..self.labels.len() {
            let idx = self.labels.len() - 1 - d;

            // --- try_table (exnref encoding) ---
            if let Some(tpc) = self.labels[idx].try_table_pc {
                let catches: &[Catch] = match &body.ir[tpc].imm {
                    Imm::TryTable(tt) => &tt.catches,
                    _ => &[],
                };
                for c in catches {
                    let matches = match c.kind {
                        CatchKind::Catch | CatchKind::CatchRef => c.tag == exn.tag,
                        CatchKind::CatchAll | CatchKind::CatchAllRef => true,
                    };
                    if !matches {
                        continue;
                    }
                    // Discard everything the try_table body pushed (including any call
                    // arguments in flight), back to its entry height. An unvalidated body may
                    // have popped BELOW that base; truncating upward would resurrect stale
                    // slots, so trap instead.
                    let base = self.labels[idx].stack_base;
                    if base > self.vstack.len() {
                        return Err(Trap::StackUnderflow);
                    }
                    self.vstack.truncate(base);
                    match c.kind {
                        CatchKind::Catch | CatchKind::CatchRef => {
                            self.vstack.extend_from_slice(&exn.values);
                        }
                        CatchKind::CatchAll | CatchKind::CatchAllRef => {}
                    }
                    if matches!(c.kind, CatchKind::CatchRef | CatchKind::CatchAllRef) {
                        let ei = store.exn_store.len();
                        if ei >= store.limits.max_exn_boxes {
                            return Err(Trap::ExnStoreExhausted);
                        }
                        store.exn_store.push(exn.clone());
                        self.push(ei as Value);
                    }
                    // The clause's label is relative to the scope ENCLOSING the try_table
                    // (§: `C ⊢ catch ok` is checked before the rule extends `C` with the
                    // block's label), so label 0 is the block one level out. The try_table's
                    // own label sits `d` deep, so branch to `d + 1 + c.label`.
                    //
                    // ⚠️ This read `d + c.label` — the same off-by-one the assembler and the
                    // validator carried, which is why all three agreed and the suite stayed
                    // green while the emitted bytes were rejected by wasmtime.
                    //
                    // `c.label` is an unvalidated `u32`, so widen before adding and reject an
                    // over-`u32` total rather than wrapping.
                    let target = d as u64 + 1 + u64::from(c.label);
                    let target = u32::try_from(target).map_err(|_| Trap::UndefinedLabel)?;
                    return self.branch(target).map(Some);
                }
            }

            // --- legacy try/catch ---
            let Some(lpc) = self.labels[idx].legacy_pc else {
                continue;
            };
            let Some(lt) = body.try_info.get(lpc).and_then(Option::as_ref) else {
                continue;
            };
            // `delegate l` re-raises "at label l", which can skip handlers this ordinary
            // outward unwind would run. The frozen oracle does not implement that routing and
            // its validator rejects `delegate` outright; we match it, and trap loudly here so
            // a hand-crafted binary that reaches a delegating try while unwinding fails
            // visibly instead of silently mis-routing.
            if lt.delegate.is_some() {
                return Err(Trap::UnsupportedInstruction);
            }
            // A throw from WITHIN this try's own handler must propagate OUTWARD rather than
            // re-match the same handler: once a handler is entered (`caught` set) we are past
            // the `catch` clause, outside the protected region. Without this guard the legacy
            // re-throw idiom `catch (e) { … throw e; }` loops forever. (`rethrow` sidesteps it
            // by popping the try before re-raising.)
            if self.labels[idx].caught.is_some() {
                continue;
            }
            for h in &lt.handlers {
                if h.tag.is_some_and(|t| t != exn.tag) {
                    continue;
                }
                let base = self.labels[idx].stack_base;
                if base > self.vstack.len() {
                    return Err(Trap::StackUnderflow);
                }
                self.vstack.truncate(base);
                if h.tag.is_some() {
                    self.vstack.extend_from_slice(&exn.values); // catch binds the payload
                }
                // Drop the body's nested labels but KEEP this try, and record the caught
                // exception so a `rethrow` naming this label can re-raise it.
                self.labels.truncate(idx + 1);
                self.labels[idx].caught = Some(exn.clone());
                return Ok(Some(h.handler_pc));
            }
        }
        Ok(None)
    }

    /// Raise `exn` from this frame: resume at the catching handler, or park it as the
    /// instance's pending exception and unwind to the caller.
    fn raise(&mut self, store: &mut Pools, exn: Exception) -> Result<usize> {
        match self.throw_exception(store, &exn)? {
            Some(target) => Ok(target),
            None => {
                store.pending_exn = Some(exn);
                Err(Trap::UncaughtException)
            }
        }
    }

    /// Handle an error propagating out of a `call`. If it is an unwinding exception this frame
    /// catches, return the resumption pc; otherwise re-raise, so a real trap — or an exception
    /// no handler here matches — keeps unwinding.
    fn on_call_error(&mut self, store: &mut Pools, e: Trap) -> Result<usize> {
        if e != Trap::UncaughtException {
            return Err(e);
        }
        // Nothing here can catch an exception it never received (a `pending_exn` of `None`
        // means the unwind did not originate from a throw on this store), so re-raise.
        let Some(exn) = store.pending_exn.take() else {
            return Err(e);
        };
        match self.throw_exception(store, &exn)? {
            Some(target) => {
                // Caught here, so the frames this unwind recorded describe a failure that did not
                // ultimately happen. Leaving them would make the NEXT trap's backtrace open with
                // an unrelated call stack.
                store.backtrace.clear();
                Ok(target)
            }
            None => {
                store.pending_exn = Some(exn); // keep unwinding outward
                Err(e)
            }
        }
    }
}

/// Branch/label arity in slots (compute slice: one slot per value).
fn block_arity(ctx: &Ctx, bt: BlockType, want_params: bool) -> u32 {
    match bt {
        BlockType::Empty => 0,
        // Both spell a single result and no parameters.
        BlockType::Value(_) | BlockType::ConcreteRef { .. } => u32::from(!want_params),
        BlockType::TypeIndex(i) => ctx.module.func_sig(i).map_or(0, |ft| {
            (if want_params {
                ft.params.len()
            } else {
                ft.results.len()
            }) as u32
        }),
    }
}

/// Call a function in instance `inst`.
///
/// Takes `code` and `pools` as **separate** borrows rather than one `&mut Store`: a
/// cross-instance call needs another `&code[callee]` while this frame still holds
/// `&mut pools`, and two immutable borrows of `code` coexist happily. That is the whole
/// reason the store is split this way — it makes wasm→wasm calls work with no `Rc`, no
/// `RefCell` and no `unsafe`.
fn call_function(
    code: &[InstanceData],
    host_funcs: &[HostFunc],
    store: &mut Pools,
    inst: usize,
    func_index: u32,
    args: &[Value],
    depth: usize,
) -> Result<Vec<Value>> {
    if depth > store.limits.max_call_depth {
        return Err(Trap::CallStackExhausted);
    }
    // 🔒 **The tail-call loop.** A `return_call*` does not recurse: `run` reports its intended callee
    // through `tail_out` and unwinds, and control comes back HERE to start the next function in the
    // same native frame at the same `depth`. That is what makes a tail call a tail call — an
    // unbounded chain uses constant native stack, and `max_call_depth` is never approached because
    // the chain never deepens. Everything below reads as before on the first iteration.
    let mut inst = inst;
    let mut func_index = func_index;
    // `None` until a tail call happens, so an ordinary call still borrows the caller's slice and
    // pays no extra allocation. Only a tail call materializes a new argument vector.
    let mut tail_args: Option<Vec<Value>> = None;
    loop {
    let args: &[Value] = tail_args.as_deref().unwrap_or(args);
    let data = code.get(inst).ok_or(Trap::UndefinedFunc)?;
    // Imported functions occupy the LOW indices of the function space, so an index below
    // the import count is an import and everything above it shifts down by that count.
    let imported = data.module.imported_func_count();
    if func_index < imported {
        return match *data.imports.get(func_index as usize).ok_or(Trap::MissingImport)? {
            FuncTarget::Host(h) => {
                let hf = host_funcs.get(h).ok_or(Trap::MissingImport)?;
                let ft = data
                    .module
                    .func_type(func_index)
                    .ok_or(Trap::UndefinedFunc)?;
                let mut results = vec![0 as Value; ft.results.len()];
                let mut caller = Caller {
                    store,
                    maps: &data.maps,
                };
                (hf.call)(&mut caller, args, &mut results)?;
                Ok(results)
            }
            // Module linking: run the callee against ITS OWN instance, so it sees the
            // exporter's memory and globals rather than the caller's.
            FuncTarget::Wasm { instance, func } => {
                call_function(code, host_funcs, store, instance, func, args, depth + 1)
            }
        };
    }
    let defined = (func_index - imported) as usize;
    let body = data.func_bodies.get(defined).ok_or(Trap::UndefinedFunc)?;

    let mut locals = vec![0 as Value; body.num_locals];
    let n_args = args.len().min(locals.len());
    locals[..n_args].copy_from_slice(&args[..n_args]);

    let mut frame = Frame {
        body,
        locals,
        vstack: Vec::new(),
        labels: vec![plain_label(false, body.ty.results.len() as u32, body.ir.len(), 0)],
    };
    let ctx = Ctx {
        module: &data.module,
        maps: &data.maps,
        code,
        host_funcs,
        inst,
    };
    // The frame is recorded HERE, not inside `run`, because this is the level that knows which
    // function is running: a `FuncBody` has no index of its own. `run` reports only the position.
    let mut pc = 0usize;
    let mut tail: Option<TailCall> = None;
    if let Err(e) = run(&mut frame, &ctx, store, depth, &mut pc, &mut tail) {
        // `pc` can sit one past the end if the body ran off its own `end`; clamp rather than
        // index, so a malformed frame degrades to a missing offset instead of a panic.
        let offset = body
            .ir
            .get(pc)
            .map_or(body.body_offset, |i| body.body_offset.saturating_add(i.offset));
        // Bounded: a backtrace cannot outgrow the call stack that produced it, and that is already
        // capped by `max_call_depth`.
        store.backtrace.push(TrapFrame {
            instance: inst,
            func_index,
            offset,
        });
        return Err(e);
    }

    // A tail call: this frame is GONE. Rebind and go round — deliberately not a recursive call,
    // and deliberately without `depth + 1`. ⚠️ The backtrace consequence is correct and worth
    // stating: a replaced frame leaves no trace, so a trap deep in a tail-call chain reports the
    // function that trapped and its real caller, not the thousands of frames that tail-called
    // through. That is what the frame having been replaced MEANS; a runtime that listed them all
    // would be describing a stack it did not keep.
    if let Some(tc) = tail {
        inst = tc.inst;
        func_index = tc.func;
        tail_args = Some(tc.args);
        continue;
    }

    let n = body.ty.results.len();
    let base = frame.stack_base(n)?;
    return Ok(frame.vstack[base..].to_vec());
    }
}

/// Run a frame to completion, reporting through `pc_out` **where** it stopped.
///
/// `pc_out` is written on every path, success or failure, and is what lets [`call_function`] name
/// the trapping instruction. It is only meaningful on the error path; on success it is simply the
/// end of the body.
///
/// `tail_out` is written **only** when the body ended in a tail call, and is how the frame gets
/// replaced rather than stacked — see [`TailCall`]. An out-parameter rather than a richer return
/// type on purpose: this function's return value is on the hot path of every call in the engine,
/// and the one previous attempt to thread state through it cost 3.6% on the steady-state benchmark
/// (see the note on `pc` below). Writing it on a cold path costs nothing.
fn run(
    frame: &mut Frame,
    ctx: &Ctx,
    store: &mut Pools,
    depth: usize,
    pc_out: &mut usize,
    tail_out: &mut Option<TailCall>,
) -> Result<()> {
    let mut pc = 0usize;
    // The loop is a closure, called once, purely so `pc` can stay a plain local while still being
    // readable after *any* exit — there are 51 `return Err` sites plus a long tail of `?`, and
    // recording the position at each is both churn and a standing invitation to add the 52nd
    // without it. The obvious spelling — `pc: &mut usize` threaded through the loop — was tried and
    // MEASURED: it cost 3.6% on the steady-state benchmark (2160 ms vs 2083 ms, A/B/A), because the
    // deref does not survive the opaque calls in the loop body. Inlined, this form keeps `pc` in a
    // register and the same benchmark does not move.
    let mut body_loop = || -> Result<()> {
        let body = frame.body;
        let ir = &body.ir;
        while pc < ir.len() {
            let instr = &ir[pc];
            match instr.op {
                Op::Nop => pc += 1,
                Op::Unreachable => return Err(Trap::Unreachable),
                Op::Drop => {
                    frame.pop();
                    pc += 1;
                }
                Op::Select | Op::SelectT => {
                    let c = frame.pop_i32();
                    let b = frame.pop();
                    let a = frame.pop();
                    frame.push(if c != 0 { a } else { b });
                    pc += 1;
                }

                // --- Constants ---
                Op::I32Const => {
                    let Imm::I32(x) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    frame.push_i32(x);
                    pc += 1;
                }
                Op::I64Const => {
                    let Imm::I64(x) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    frame.push_i64(x);
                    pc += 1;
                }
                Op::F32Const => {
                    let Imm::F32(bits) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    frame.push(Value::from(bits));
                    pc += 1;
                }
                Op::F64Const => {
                    let Imm::F64(bits) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    frame.push(Value::from(bits));
                    pc += 1;
                }

                // --- Variables ---
                Op::LocalGet => {
                    let i = local_index(instr)?;
                    let v = *frame.locals.get(i).ok_or(Trap::StackUnderflow)?;
                    frame.push(v);
                    pc += 1;
                }
                Op::LocalSet => {
                    let i = local_index(instr)?;
                    let v = frame.pop();
                    *frame.locals.get_mut(i).ok_or(Trap::StackUnderflow)? = v;
                    pc += 1;
                }
                Op::LocalTee => {
                    let i = local_index(instr)?;
                    let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?;
                    *frame.locals.get_mut(i).ok_or(Trap::StackUnderflow)? = v;
                    pc += 1;
                }
                Op::GlobalGet => {
                    let Imm::Global(gi) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = *store.globals.get(ctx.maps.global(gi)).ok_or(Trap::UndefinedGlobal)?;
                    frame.push(v);
                    pc += 1;
                }
                Op::GlobalSet => {
                    let Imm::Global(gi) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = frame.pop();
                    *store.globals.get_mut(ctx.maps.global(gi)).ok_or(Trap::UndefinedGlobal)? = v;
                    pc += 1;
                }

                // --- Structured control flow ---
                Op::Block => {
                    let bt = block_type(instr)?;
                    let params = block_arity(ctx, bt, true);
                    let arity = block_arity(ctx, bt, false);
                    let stack_base = frame.stack_base(params as usize)?;
                    frame
                        .labels
                        .push(plain_label(false, arity, body.end_of[pc] + 1, stack_base));
                    pc += 1;
                }
                Op::Loop => {
                    let bt = block_type(instr)?;
                    let params = block_arity(ctx, bt, true);
                    let stack_base = frame.stack_base(params as usize)?;
                    frame
                        .labels
                        .push(plain_label(true, params, pc + 1, stack_base));
                    pc += 1;
                }
                Op::If => {
                    let c = frame.pop_i32();
                    let bt = block_type(instr)?;
                    let params = block_arity(ctx, bt, true);
                    let arity = block_arity(ctx, bt, false);
                    let stack_base = frame.stack_base(params as usize)?;
                    frame
                        .labels
                        .push(plain_label(false, arity, body.end_of[pc] + 1, stack_base));
                    if c != 0 {
                        pc += 1;
                    } else {
                        let else_idx = body.else_of[pc];
                        pc = if else_idx != ir.len() {
                            else_idx + 1
                        } else {
                            body.end_of[pc]
                        };
                    }
                }
                Op::Else => pc = body.end_of[pc], // end of then-branch: skip to matching end
                Op::End => {
                    frame.labels.pop();
                    pc += 1;
                }

                // --- Exception handling: try_table (exnref encoding) ---
                Op::TryTable => {
                    let Imm::TryTable(tt) = &instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let params = block_arity(ctx, tt.block_type, true);
                    let arity = block_arity(ctx, tt.block_type, false);
                    let stack_base = frame.stack_base(params as usize)?;
                    frame.labels.push(Label {
                        is_loop: false,
                        arity,
                        target: body.end_of[pc] + 1,
                        stack_base,
                        try_table_pc: Some(pc),
                        legacy_pc: None,
                        caught: None,
                    });
                    pc += 1;
                }
                Op::Throw => {
                    let tag = tag_imm(instr)?;
                    let ft = ctx.module.tag_type(tag).ok_or(Trap::UndefinedTag)?;
                    let base = frame.stack_base(ft.params.len())?;
                    let values = frame.vstack[base..].to_vec();
                    frame.vstack.truncate(base);
                    pc = frame.raise(store, Exception { tag, values })?;
                }
                Op::ThrowRef => {
                    let r = frame.pop();
                    if r == NULL_REF {
                        return Err(Trap::NullReference);
                    }
                    let ei = usize::try_from(r).map_err(|_| Trap::NullReference)?;
                    // An out-of-range exnref is only reachable from an unvalidated module.
                    let exn = store
                        .exn_store
                        .get(ei)
                        .ok_or(Trap::NullReference)?
                        .clone();
                    pc = frame.raise(store, exn)?;
                }

                // --- Exception handling: the legacy try/catch encoding ---
                Op::TryLegacy => {
                    let bt = block_type(instr)?;
                    let params = block_arity(ctx, bt, true);
                    let arity = block_arity(ctx, bt, false);
                    let stack_base = frame.stack_base(params as usize)?;
                    frame.labels.push(Label {
                        is_loop: false,
                        arity,
                        target: body.end_of[pc] + 1,
                        stack_base,
                        try_table_pc: None,
                        legacy_pc: Some(pc),
                        caught: None,
                    });
                    pc += 1;
                }
                // Reached only by normal control flow (the body, or a prior handler, completed):
                // skip the remaining handlers to the `end`.
                Op::CatchLegacy | Op::CatchAll => pc = body.end_of[pc],
                // `delegate` reached by normal flow just ends its try, like `end`.
                Op::Delegate => {
                    frame.labels.pop();
                    pc += 1;
                }
                Op::Rethrow => {
                    // Re-raise the exception caught by the try `n` levels out, propagating from
                    // OUTSIDE that try — it already had its turn at this exception.
                    let n = label_imm(instr)? as usize;
                    if n >= frame.labels.len() {
                        return Err(Trap::UndefinedLabel);
                    }
                    let idx = frame.labels.len() - 1 - n;
                    let exn = frame.labels[idx]
                        .caught
                        .clone()
                        .ok_or(Trap::UncaughtException)?;
                    let base = frame.labels[idx].stack_base;
                    frame.labels.truncate(idx);
                    if base > frame.vstack.len() {
                        return Err(Trap::StackUnderflow);
                    }
                    frame.vstack.truncate(base);
                    pc = frame.raise(store, exn)?;
                }
                Op::Br => pc = frame.branch(label_imm(instr)?)?,
                Op::BrIf => {
                    if frame.pop_i32() != 0 {
                        pc = frame.branch(label_imm(instr)?)?;
                    } else {
                        pc += 1;
                    }
                }
                Op::BrTable => {
                    let Imm::BrTable(bt) = &instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let i = frame.pop_i32();
                    let idx = if i >= 0 && (i as usize) < bt.labels.len() {
                        bt.labels[i as usize]
                    } else {
                        bt.default
                    };
                    pc = frame.branch(idx)?;
                }
                Op::Return => pc = ir.len(),

                // §4.4.8 `return_call x`. The frame is REPLACED: take the callee's arguments and end
                // the body, reporting the target rather than calling it. `call_function` loops.
                Op::ReturnCall => {
                    let Imm::Func(f) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let ft = ctx.module.func_type(f).ok_or(Trap::UndefinedFunc)?;
                    let base = frame.stack_base(ft.params.len())?;
                    *tail_out = Some(TailCall {
                        inst: ctx.inst,
                        func: f,
                        args: frame.vstack[base..].to_vec(),
                    });
                    pc = ir.len();
                }
                Op::Call => {
                    let Imm::Func(f) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let ft = ctx.module.func_type(f).ok_or(Trap::UndefinedFunc)?;
                    let np = ft.params.len();
                    let base = frame.stack_base(np)?;
                    let args = frame.vstack[base..].to_vec();
                    let results = match call_function(ctx.code, ctx.host_funcs, store, ctx.inst, f, &args, depth + 1) {
                        Ok(r) => r,
                        Err(e) => {
                            // An exception unwinding out of the callee may be caught here.
                            pc = frame.on_call_error(store, e)?;
                            continue;
                        }
                    };
                    frame.vstack.truncate(base);
                    frame.vstack.extend_from_slice(&results);
                    pc += 1;
                }

                // --- Linear memory (loads/stores, size/grow) ---
                Op::I32Load
                | Op::I64Load
                | Op::F32Load
                | Op::F64Load
                | Op::I32Load8S
                | Op::I32Load8U
                | Op::I32Load16S
                | Op::I32Load16U
                | Op::I64Load8S
                | Op::I64Load8U
                | Op::I64Load16S
                | Op::I64Load16U
                | Op::I64Load32S
                | Op::I64Load32U
                | Op::I32Store
                | Op::I64Store
                | Op::F32Store
                | Op::F64Store
                | Op::I32Store8
                | Op::I32Store16
                | Op::I64Store8
                | Op::I64Store16
                | Op::I64Store32
                | Op::MemorySize
                | Op::MemoryGrow => {
                    exec_memory(frame, store, ctx.maps, instr)?;
                    pc += 1;
                }
                Op::MemoryCopy => {
                    exec_memory_copy(frame, store, ctx.maps, instr)?;
                    pc += 1;
                }
                Op::MemoryFill => {
                    exec_memory_fill(frame, store, ctx.maps, instr)?;
                    pc += 1;
                }
                Op::MemoryInit => {
                    exec_memory_init(frame, ctx, store, instr)?;
                    pc += 1;
                }
                Op::DataDrop => {
                    let Imm::Data(d) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    *store
                        .data_dropped
                        .get_mut(ctx.maps.data(d))
                        .ok_or(Trap::UndefinedData)? = true;
                    pc += 1;
                }

                // --- Reference types --- (funcref = function index; NULL_REF = null)
                Op::RefNull => {
                    frame.push(NULL_REF);
                    pc += 1;
                }
                Op::RefIsNull => {
                    let r = frame.pop();
                    frame.push_i32(i32::from(r == NULL_REF));
                    pc += 1;
                }
                Op::RefFunc => {
                    let Imm::Func(f) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    // Stamped with the producing instance, so the reference stays callable correctly
                    // after it is stored into a table another instance also holds.
                    frame.push(pack_funcref(ctx.inst, f));
                    pc += 1;
                }
                Op::RefAsNonNull => {
                    let r = frame.pop();
                    if r == NULL_REF {
                        return Err(Trap::NullReference);
                    }
                    frame.push(r);
                    pc += 1;
                }
                Op::BrOnNull => {
                    let r = frame.pop();
                    if r == NULL_REF {
                        pc = frame.branch(label_imm(instr)?)?; // null → branch (ref dropped)
                    } else {
                        frame.push(r); // non-null → keep the ref, fall through
                        pc += 1;
                    }
                }
                Op::BrOnNonNull => {
                    let r = frame.pop();
                    if r == NULL_REF {
                        pc += 1; // null → ref consumed, fall through
                    } else {
                        frame.push(r); // non-null → keep the ref for the label
                        pc = frame.branch(label_imm(instr)?)?;
                    }
                }
                Op::CallRef | Op::ReturnCallRef => {
                    let f_ref = frame.pop();
                    if f_ref == NULL_REF {
                        return Err(Trap::NullReference);
                    }
                    // Resolved against the funcref's OWN instance, not the caller's. For a reference
                    // produced in this instance the two are the same; for one that arrived through a
                    // shared table they are not, and using the caller's would be the wrong function.
                    let owner = funcref_instance(f_ref);
                    let f = funcref_index(f_ref);
                    let ft = ctx
                        .code
                        .get(owner)
                        .and_then(|d| d.module.func_type(f))
                        .ok_or(Trap::UndefinedFunc)?;
                    let base = frame.stack_base(ft.params.len())?;
                    // ⚠️ `return_call_ref` used to be implemented here as "call, then jump to the end
                    // of the body" — which returns the right answer and is NOT a tail call: it
                    // recursed into `call_function`, so the native stack grew exactly as it does for
                    // an ordinary call, and unbounded mutual recursion still exhausted it. The whole
                    // point of the proposal is that it does not. It now reports the target instead.
                    if instr.op == Op::ReturnCallRef {
                        *tail_out = Some(TailCall {
                            inst: owner,
                            func: f,
                            args: frame.vstack[base..].to_vec(),
                        });
                        pc = ir.len();
                        continue;
                    }
                    let args = frame.vstack[base..].to_vec();
                    let results = match call_function(ctx.code, ctx.host_funcs, store, owner, f, &args, depth + 1) {
                        Ok(r) => r,
                        Err(e) => {
                            pc = frame.on_call_error(store, e)?;
                            continue;
                        }
                    };
                    frame.vstack.truncate(base);
                    frame.vstack.extend_from_slice(&results);
                    pc += 1;
                }

                // --- call_indirect: table lookup + runtime type check ---
                // `return_call_indirect` shares every step — the table read, the null and bounds
                // traps, and the type-identity check — and differs only in what it does with the
                // resolved target, so it is deliberately the SAME arm rather than a copy.
                Op::CallIndirect | Op::ReturnCallIndirect => {
                    let Imm::CallIndirect(ci) = &instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let slot = frame.pop_i32() as u32 as usize;
                    let entry = *store
                        .tables
                        .get(ctx.maps.table(ci.table))
                        .ok_or(Trap::NoTable)?
                        .entries
                        .get(slot)
                        .ok_or(Trap::TableOutOfBounds)?;
                    if entry == NULL_REF {
                        return Err(Trap::UninitializedElement);
                    }
                    // Unpacked, so a reference that arrived through a table another instance also holds
                    // dispatches to ITS function — the whole reason a funcref carries its owner. Before
                    // this, `entry as u32` resolved against the *calling* instance: a silent wrong call.
                    let owner = funcref_instance(entry);
                    let f = funcref_index(entry);
                    let want = ctx.module.func_sig(ci.type_index).ok_or(Trap::UndefinedType)?;
                    let owner_data = ctx.code.get(owner).ok_or(Trap::UndefinedFunc)?;
                    let got = owner_data.module.func_type(f).ok_or(Trap::UndefinedFunc)?;
                    // Matched on **type identity with subtyping**, by index — not by comparing the two
                    // signatures' shapes. Same lesson as import matching, and the third site to need it:
                    // two functions can both be `(func)` and still be different types, because rec-group
                    // membership is part of identity and only the index carries it. §4.4.8 wants the
                    // callee's type to be a *subtype* of the declared one, which is also why equality was
                    // wrong in the permissive direction (`assert_trap` cases returning a result).
                    //
                    // When the callee belongs to ANOTHER instance the two type indices are in different
                    // modules, so they are compared through the store-wide registry — the same mechanism
                    // import matching uses. Same-instance stays on the cheaper module-local path.
                    let matched = if owner == ctx.inst {
                        match ctx.module.func_type_index(f) {
                            Some(got_ti) => ctx.module.is_subtype(got_ti, ci.type_index),
                            // An *imported* function has no defining type index in this module. Fall back
                            // to the structural comparison, which `func_types_equal` decides canonically.
                            None => ctx.module.func_types_equal(&want, &got),
                        }
                    } else {
                        match (
                            ctx.code[ctx.inst]
                                .type_ids
                                .get(ci.type_index as usize)
                                .copied(),
                            owner_data
                                .module
                                .func_type_index(f)
                                .and_then(|ti| owner_data.type_ids.get(ti as usize).copied()),
                        ) {
                            (Some(want_id), Some(got_id)) => store.types.is_subtype(got_id, want_id),
                            // No store-wide id on one side (a re-exported import has no defining type
                            // index): fall back to the structural comparison rather than refusing.
                            _ => ctx.module.func_types_equal(&want, &got),
                        }
                    };
                    if !matched {
                        return Err(Trap::IndirectTypeMismatch);
                    }
                    let base = frame.stack_base(got.params.len())?;
                    // The tail form replaces the frame instead of stacking one — and note it inherits
                    // the cross-instance `owner` above, so a tail call THROUGH a shared table lands in
                    // the reference's own instance, same as the non-tail form.
                    if instr.op == Op::ReturnCallIndirect {
                        *tail_out = Some(TailCall {
                            inst: owner,
                            func: f,
                            args: frame.vstack[base..].to_vec(),
                        });
                        pc = ir.len();
                        continue;
                    }
                    let args = frame.vstack[base..].to_vec();
                    // Into the funcref's OWN instance, so the callee runs against its own memory,
                    // globals and tables. `ctx.inst` here was the silent-wrong-call.
                    let results = match call_function(ctx.code, ctx.host_funcs, store, owner, f, &args, depth + 1) {
                        Ok(r) => r,
                        Err(e) => {
                            pc = frame.on_call_error(store, e)?;
                            continue;
                        }
                    };
                    frame.vstack.truncate(base);
                    frame.vstack.extend_from_slice(&results);
                    pc += 1;
                }

                // --- Table access ---
                Op::TableGet => {
                    let ti = ctx.maps.table(table_imm(instr)?);
                    let i = frame.pop_i32() as u32 as usize;
                    let v = *store
                        .tables
                        .get(ti)
                        .ok_or(Trap::NoTable)?
                        .entries
                        .get(i)
                        .ok_or(Trap::TableOutOfBounds)?;
                    frame.push(v);
                    pc += 1;
                }
                Op::TableSet => {
                    let ti = ctx.maps.table(table_imm(instr)?);
                    let v = frame.pop();
                    let i = frame.pop_i32() as u32 as usize;
                    let slot = store
                        .tables
                        .get_mut(ti)
                        .ok_or(Trap::NoTable)?
                        .entries
                        .get_mut(i)
                        .ok_or(Trap::TableOutOfBounds)?;
                    *slot = v;
                    pc += 1;
                }
                Op::TableSize => {
                    let ti = ctx.maps.table(table_imm(instr)?);
                    let len = store.tables.get(ti).ok_or(Trap::NoTable)?.entries.len();
                    frame.push_i32(len as i32);
                    pc += 1;
                }
                Op::TableGrow => {
                    let ti = ctx.maps.table(table_imm(instr)?);
                    let delta = frame.pop_i32() as u32 as usize;
                    let init = frame.pop();
                    // Read the ceiling before borrowing the table: `limits` and `tables` are
                    // sibling fields, so taking `&mut` on one would block reading the other.
                    let ceiling = store.limits.max_table_elems;
                    let table = store.tables.get_mut(ti).ok_or(Trap::NoTable)?;
                    let old = table.entries.len();
                    let limit = table.max.map_or(ceiling, |m| m as usize).min(ceiling);
                    match old.checked_add(delta).filter(|&n| n <= limit) {
                        Some(new_len) => {
                            table.entries.resize(new_len, init);
                            frame.push_i32(old as i32);
                        }
                        None => frame.push_i32(-1), // growth refused
                    }
                    pc += 1;
                }
                Op::TableFill => {
                    let ti = ctx.maps.table(table_imm(instr)?);
                    let n = frame.pop_i32() as u32 as usize;
                    let val = frame.pop();
                    let dst = frame.pop_i32() as u32 as usize;
                    let table = store.tables.get_mut(ti).ok_or(Trap::NoTable)?;
                    let end = dst.checked_add(n).filter(|&e| e <= table.entries.len());
                    let end = end.ok_or(Trap::TableOutOfBounds)?;
                    table.entries[dst..end].fill(val);
                    pc += 1;
                }
                Op::TableInit => {
                    let Imm::TableInit { elem, table } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let (ei, ti) = (ctx.maps.elem(elem), ctx.maps.table(table));
                    let dropped = *store.elem_dropped.get(ei).ok_or(Trap::UndefinedElement)?;
                    let n = frame.pop_i32() as u32 as usize;
                    let src = frame.pop_i32() as u32 as usize;
                    let dst = frame.pop_i32() as u32 as usize;
                    let seg_len = if dropped { 0 } else { store.elem_values[ei].len() };
                    let tbl_len = store.tables.get(ti).ok_or(Trap::NoTable)?.entries.len();
                    if src.checked_add(n).is_none_or(|e| e > seg_len)
                        || dst.checked_add(n).is_none_or(|e| e > tbl_len)
                    {
                        return Err(Trap::TableOutOfBounds);
                    }
                    for k in 0..n {
                        let v = if dropped {
                            NULL_REF
                        } else {
                            store.elem_values[ei][src + k]
                        };
                        store.tables[ti].entries[dst + k] = v;
                    }
                    pc += 1;
                }
                Op::ElemDrop => {
                    let Imm::Elem(e) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    *store
                        .elem_dropped
                        .get_mut(ctx.maps.elem(e))
                        .ok_or(Trap::UndefinedElement)? = true;
                    pc += 1;
                }
                Op::TableCopy => {
                    let Imm::TableCopy { dst, src } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let (di, si) = (ctx.maps.table(dst), ctx.maps.table(src));
                    let n = frame.pop_i32() as u32 as usize;
                    let s = frame.pop_i32() as u32 as usize;
                    let d = frame.pop_i32() as u32 as usize;
                    let src_len = store.tables.get(si).ok_or(Trap::NoTable)?.entries.len();
                    let dst_len = store.tables.get(di).ok_or(Trap::NoTable)?.entries.len();
                    if s.checked_add(n).is_none_or(|e| e > src_len)
                        || d.checked_add(n).is_none_or(|e| e > dst_len)
                    {
                        return Err(Trap::TableOutOfBounds);
                    }
                    if di == si {
                        store.tables[di].entries.copy_within(s..s + n, d);
                    } else {
                        let tmp = store.tables[si].entries[s..s + n].to_vec();
                        store.tables[di].entries[d..d + n].copy_from_slice(&tmp);
                    }
                    pc += 1;
                }

                // --- WasmGC: the externref bridge (§4.4.7.3) ---
                Op::AnyConvertExtern => {
                    let r = frame.pop();
                    frame.push(internalize(r));
                    pc += 1;
                }
                Op::ExternConvertAny => {
                    let r = frame.pop();
                    frame.push(externalize(r));
                    pc += 1;
                }

                // --- WasmGC: i31 (unboxed; i31_tag checked AFTER null_ref) ---
                Op::RefI31 => {
                    let x = frame.pop_i32() as u32;
                    frame.push(I31_TAG | Value::from(x & 0x7fff_ffff)); // wrap to 31 bits, non-null
                    pc += 1;
                }
                Op::I31GetS => {
                    let r = frame.pop();
                    if r == NULL_REF {
                        return Err(Trap::NullReference);
                    }
                    let n = r as u32;
                    frame.push_i32(((n << 1) as i32) >> 1); // sign-extend the 31-bit payload
                    pc += 1;
                }
                Op::I31GetU => {
                    let r = frame.pop();
                    if r == NULL_REF {
                        return Err(Trap::NullReference);
                    }
                    frame.push_i32((r as u32 & 0x7fff_ffff) as i32);
                    pc += 1;
                }
                Op::RefEq => {
                    let b = frame.pop();
                    let a = frame.pop();
                    frame.push_i32(i32::from(a == b));
                    pc += 1;
                }

                // --- WasmGC: struct objects ---
                Op::StructNew => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let sf = ctx.module.struct_fields(ti).ok_or(Trap::UndefinedType)?;
                    let base = frame.stack_base(sf.len())?;
                    let obj: Vec<Value> = sf
                        .iter()
                        .enumerate()
                        .map(|(k, f)| pack_field(f.storage, frame.vstack[base + k]))
                        .collect();
                    frame.vstack.truncate(base);
                    let r = alloc_object(store, ctx.inst, ti, obj)?;
                    frame.push(r);
                    pc += 1;
                }
                Op::StructNewDefault => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let sf = ctx.module.struct_fields(ti).ok_or(Trap::UndefinedType)?;
                    let obj: Vec<Value> = sf.iter().map(|f| default_field(f.storage)).collect();
                    let r = alloc_object(store, ctx.inst, ti, obj)?;
                    frame.push(r);
                    pc += 1;
                }
                Op::StructGet | Op::StructGetS | Op::StructGetU => {
                    let Imm::GcField { type_index, field } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let idx = gc_object_index(store, frame.pop())?;
                    let storage = ctx
                        .module
                        .struct_fields(type_index)
                        .ok_or(Trap::UndefinedType)?
                        .get(field as usize)
                        .ok_or(Trap::GcOutOfBounds)?
                        .storage;
                    let v = *store.gc_heap[idx]
                        .fields
                        .get(field as usize)
                        .ok_or(Trap::GcOutOfBounds)?;
                    frame.push(unpack_field(storage, v, instr.op == Op::StructGetS));
                    pc += 1;
                }
                Op::StructSet => {
                    let Imm::GcField { type_index, field } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = frame.pop();
                    let idx = gc_object_index(store, frame.pop())?;
                    let storage = ctx
                        .module
                        .struct_fields(type_index)
                        .ok_or(Trap::UndefinedType)?
                        .get(field as usize)
                        .ok_or(Trap::GcOutOfBounds)?
                        .storage;
                    let slot = store.gc_heap[idx]
                        .fields
                        .get_mut(field as usize)
                        .ok_or(Trap::GcOutOfBounds)?;
                    *slot = pack_field(storage, v);
                    pc += 1;
                }

                // --- WasmGC: array objects ---
                Op::ArrayNew => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                    let len = frame.pop_i32() as u32 as usize;
                    let init = pack_field(f.storage, frame.pop());
                    let r = alloc_object(store, ctx.inst, ti, vec![init; len])?;
                    frame.push(r);
                    pc += 1;
                }
                Op::ArrayNewDefault => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                    let len = frame.pop_i32() as u32 as usize;
                    let r = alloc_object(store, ctx.inst, ti, vec![default_field(f.storage); len])?;
                    frame.push(r);
                    pc += 1;
                }
                Op::ArrayNewFixed => {
                    let Imm::GcTypeN { type_index, n } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let f = ctx.module.array_field(type_index).ok_or(Trap::UndefinedType)?;
                    let base = frame.stack_base(n as usize)?;
                    let obj: Vec<Value> = (0..n as usize)
                        .map(|k| pack_field(f.storage, frame.vstack[base + k]))
                        .collect();
                    frame.vstack.truncate(base);
                    let r = alloc_object(store, ctx.inst, type_index, obj)?;
                    frame.push(r);
                    pc += 1;
                }
                Op::ArrayGet | Op::ArrayGetS | Op::ArrayGetU => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                    let index = frame.pop_i32() as u32 as usize;
                    let idx = gc_object_index(store, frame.pop())?;
                    let v = *store.gc_heap[idx]
                        .fields
                        .get(index)
                        .ok_or(Trap::GcOutOfBounds)?;
                    frame.push(unpack_field(f.storage, v, instr.op == Op::ArrayGetS));
                    pc += 1;
                }
                Op::ArraySet => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                    let v = frame.pop();
                    let index = frame.pop_i32() as u32 as usize;
                    let idx = gc_object_index(store, frame.pop())?;
                    let slot = store.gc_heap[idx]
                        .fields
                        .get_mut(index)
                        .ok_or(Trap::GcOutOfBounds)?;
                    *slot = pack_field(f.storage, v);
                    pc += 1;
                }
                Op::ArrayLen => {
                    let idx = gc_object_index(store, frame.pop())?;
                    frame.push_i32(store.gc_heap[idx].fields.len() as i32);
                    pc += 1;
                }
                // --- WasmGC array bulk ops (added 2026-08-19) ---
                Op::ArrayNewData | Op::ArrayNewElem => {
                    exec_array_new_seg(frame, ctx, store, instr)?;
                    pc += 1;
                }
                Op::ArrayFill => {
                    let Imm::GcType(ti) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let f = ctx.module.array_field(ti).ok_or(Trap::UndefinedType)?;
                    let n = frame.pop_i32() as u32 as usize;
                    let v = pack_field(f.storage, frame.pop());
                    let off = frame.pop_i32() as u32 as usize;
                    let idx = gc_object_index(store, frame.pop())?;
                    let len = store.gc_heap[idx].fields.len();
                    // Bounds are checked BEFORE any write, so a partially-applied fill can
                    // never be observed — the spec traps without side effects.
                    if off.checked_add(n).is_none_or(|end| end > len) {
                        return Err(Trap::GcOutOfBounds);
                    }
                    for k in 0..n {
                        store.gc_heap[idx].fields[off + k] = v;
                    }
                    pc += 1;
                }
                Op::ArrayCopy => {
                    let Imm::GcArrayCopy { dst, src } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let df = ctx.module.array_field(dst).ok_or(Trap::UndefinedType)?;
                    let sf = ctx.module.array_field(src).ok_or(Trap::UndefinedType)?;
                    let n = frame.pop_i32() as u32 as usize;
                    let soff = frame.pop_i32() as u32 as usize;
                    let sidx = gc_object_index(store, frame.pop())?;
                    let doff = frame.pop_i32() as u32 as usize;
                    let didx = gc_object_index(store, frame.pop())?;
                    let (dlen, slen) =
                        (store.gc_heap[didx].fields.len(), store.gc_heap[sidx].fields.len());
                    if doff.checked_add(n).is_none_or(|e| e > dlen)
                        || soff.checked_add(n).is_none_or(|e| e > slen)
                    {
                        return Err(Trap::GcOutOfBounds);
                    }
                    // Read the whole source run first: the two arrays may be the SAME object,
                    // and an overlapping forward copy would otherwise read values it just wrote.
                    let run: Vec<Value> = (0..n)
                        .map(|k| {
                            let raw = store.gc_heap[sidx].fields[soff + k];
                            // Re-pack through the destination's storage: a packed source read
                            // as i32 must be re-narrowed if the destination is narrower.
                            pack_field(df.storage, unpack_field(sf.storage, raw, false))
                        })
                        .collect();
                    for (k, v) in run.into_iter().enumerate() {
                        store.gc_heap[didx].fields[doff + k] = v;
                    }
                    pc += 1;
                }
                Op::ArrayInitData | Op::ArrayInitElem => {
                    exec_array_init_seg(frame, ctx, store, instr)?;
                    pc += 1;
                }

                // --- WasmGC: casts ---
                Op::RefTest => {
                    let Imm::RefCast(rt) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = frame.pop();
                    frame.push_i32(i32::from(ref_matches(ctx.module, ctx.code, store, ctx.inst, v, rt)));
                    pc += 1;
                }
                Op::RefCastOp => {
                    let Imm::RefCast(rt) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?; // peek — value stays
                    if !ref_matches(ctx.module, ctx.code, store, ctx.inst, v, rt) {
                        return Err(Trap::CastFailure);
                    }
                    pc += 1;
                }
                Op::BrOnCast => {
                    let Imm::BrCast { label, dst, .. } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?;
                    pc = if ref_matches(ctx.module, ctx.code, store, ctx.inst, v, dst) {
                        frame.branch(label)?
                    } else {
                        pc + 1
                    };
                }
                Op::BrOnCastFail => {
                    let Imm::BrCast { label, dst, .. } = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    let v = *frame.vstack.last().ok_or(Trap::StackUnderflow)?;
                    pc = if ref_matches(ctx.module, ctx.code, store, ctx.inst, v, dst) {
                        pc + 1
                    } else {
                        frame.branch(label)?
                    };
                }

                // --- SIMD (v128, 0xFD family) ---
                Op::Simd => {
                    let Imm::Simd(s) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    exec_simd(frame, store, ctx.maps, s)?;
                    pc += 1;
                }

                // --- Threads / atomics (0xFE family) ---
                Op::Atomic => {
                    let Imm::Atomic(at) = instr.imm else {
                        return Err(Trap::UnsupportedInstruction);
                    };
                    exec_atomic(frame, store, ctx.maps, at)?;
                    pc += 1;
                }

                // Integer arithmetic / comparison / bitwise / conversion.
                _ => {
                    exec_numeric(frame, instr.op)?;
                    pc += 1;
                }
            }
        }
        Ok(())
    };
    let r = body_loop();
    *pc_out = pc;
    r
}

fn local_index(instr: &Instr) -> Result<usize> {
    if let Imm::Local(i) = instr.imm {
        Ok(i as usize)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn label_imm(instr: &Instr) -> Result<u32> {
    if let Imm::Label(l) = instr.imm {
        Ok(l)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn block_type(instr: &Instr) -> Result<BlockType> {
    if let Imm::BlockType(bt) = instr.imm {
        Ok(bt)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn tag_imm(instr: &Instr) -> Result<u32> {
    if let Imm::Tag(t) = instr.imm {
        Ok(t)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}
fn table_imm(instr: &Instr) -> Result<u32> {
    if let Imm::Table(t) = instr.imm {
        Ok(t)
    } else {
        Err(Trap::UnsupportedInstruction)
    }
}

/// Overflow-safe range check: is `base + n <= len`? Returns `base` as `usize` (then `< len`,
/// so it fits) or `None` for out-of-bounds.
fn mem_range(base: u64, n: u64, len: usize) -> Option<usize> {
    if base.checked_add(n)? > len as u64 {
        return None;
    }
    usize::try_from(base).ok()
}

/// Read `n` (1..=8) bytes little-endian from memory `ma.memory` at (popped address + offset),
/// zero-extended into a `u64`. The caller sign/zero-extends into the target slot.
fn load_bytes(frame: &mut Frame, store: &Pools, maps: &IndexMaps, ma: crate::opcode::MemArg, n: usize) -> Result<u64> {
    let mem = store.memories.get(maps.mem(ma.memory)).ok_or(Trap::NoMemory)?;
    let addr = frame.pop_mem(mem.is64);
    let ea = addr.checked_add(ma.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(n as u64).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    let start = ea as usize;
    let mut v = 0u64;
    for (i, &b) in mem.bytes[start..start + n].iter().enumerate() {
        v |= u64::from(b) << (8 * i);
    }
    Ok(v)
}

/// Write the low `n` bytes of `val` little-endian to memory `ma.memory`. The value was already
/// popped by the caller; this pops the address.
fn store_bytes(
    frame: &mut Frame,
    store: &mut Pools,
    maps: &IndexMaps,
    ma: crate::opcode::MemArg,
    n: usize,
    val: u64,
) -> Result<()> {
    let mem = store.memories.get_mut(maps.mem(ma.memory)).ok_or(Trap::NoMemory)?;
    let addr = frame.pop_mem(mem.is64);
    let ea = addr.checked_add(ma.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(n as u64).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    let start = ea as usize;
    for (i, b) in mem.bytes[start..start + n].iter_mut().enumerate() {
        *b = (val >> (8 * i)) as u8;
    }
    Ok(())
}

/// Loads/stores + `memory.size`/`grow`.
fn exec_memory(frame: &mut Frame, store: &mut Pools, maps: &IndexMaps, instr: &Instr) -> Result<()> {
    match instr.op {
        Op::MemorySize => {
            let Imm::MemIndex(mi) = instr.imm else {
                return Err(Trap::UnsupportedInstruction);
            };
            // Route the module-local immediate through `maps` — a raw index into the
            // shared pool reads whichever instance happens to sit there (the shared-store
            // defect class; invisible with one instance per store, where they are equal).
            let mem = store.memories.get(maps.mem(mi)).ok_or(Trap::NoMemory)?;
            let pages = (mem.bytes.len() / PAGE_SIZE) as u64;
            if mem.is64 {
                frame.push_i64(pages as i64);
            } else {
                frame.push_i32(pages as i32);
            }
            return Ok(());
        }
        Op::MemoryGrow => return memory_grow(frame, store, maps, instr),
        _ => {}
    }
    let Imm::Mem(ma) = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    match instr.op {
        Op::I32Load => {
            let v = load_bytes(frame, store, maps, ma, 4)?;
            frame.push_i32(v as u32 as i32);
        }
        Op::I64Load => {
            let v = load_bytes(frame, store, maps, ma, 8)?;
            frame.push_i64(v as i64);
        }
        Op::F32Load => {
            let v = load_bytes(frame, store, maps, ma, 4)?;
            frame.push(Value::from(v));
        }
        Op::F64Load => {
            let v = load_bytes(frame, store, maps, ma, 8)?;
            frame.push(Value::from(v));
        }
        Op::I32Load8S => {
            let v = load_bytes(frame, store, maps, ma, 1)?;
            frame.push_i32(i32::from(v as u8 as i8));
        }
        Op::I32Load8U => {
            let v = load_bytes(frame, store, maps, ma, 1)?;
            frame.push_i32(i32::from(v as u8));
        }
        Op::I32Load16S => {
            let v = load_bytes(frame, store, maps, ma, 2)?;
            frame.push_i32(i32::from(v as u16 as i16));
        }
        Op::I32Load16U => {
            let v = load_bytes(frame, store, maps, ma, 2)?;
            frame.push_i32(i32::from(v as u16));
        }
        Op::I64Load8S => {
            let v = load_bytes(frame, store, maps, ma, 1)?;
            frame.push_i64(i64::from(v as u8 as i8));
        }
        Op::I64Load8U => {
            let v = load_bytes(frame, store, maps, ma, 1)?;
            frame.push_i64(i64::from(v as u8));
        }
        Op::I64Load16S => {
            let v = load_bytes(frame, store, maps, ma, 2)?;
            frame.push_i64(i64::from(v as u16 as i16));
        }
        Op::I64Load16U => {
            let v = load_bytes(frame, store, maps, ma, 2)?;
            frame.push_i64(i64::from(v as u16));
        }
        Op::I64Load32S => {
            let v = load_bytes(frame, store, maps, ma, 4)?;
            frame.push_i64(i64::from(v as u32 as i32));
        }
        Op::I64Load32U => {
            let v = load_bytes(frame, store, maps, ma, 4)?;
            frame.push_i64(i64::from(v as u32));
        }
        Op::I32Store => {
            let val = u64::from(frame.pop_i32() as u32);
            store_bytes(frame, store, maps, ma, 4, val)?;
        }
        Op::I64Store => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, maps, ma, 8, val)?;
        }
        Op::F32Store => {
            let val = frame.pop() as u64 & 0xffff_ffff;
            store_bytes(frame, store, maps, ma, 4, val)?;
        }
        Op::F64Store => {
            let val = frame.pop() as u64;
            store_bytes(frame, store, maps, ma, 8, val)?;
        }
        Op::I32Store8 => {
            let val = u64::from(frame.pop_i32() as u32);
            store_bytes(frame, store, maps, ma, 1, val)?;
        }
        Op::I32Store16 => {
            let val = u64::from(frame.pop_i32() as u32);
            store_bytes(frame, store, maps, ma, 2, val)?;
        }
        Op::I64Store8 => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, maps, ma, 1, val)?;
        }
        Op::I64Store16 => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, maps, ma, 2, val)?;
        }
        Op::I64Store32 => {
            let val = frame.pop_i64() as u64;
            store_bytes(frame, store, maps, ma, 4, val)?;
        }
        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

fn memory_grow(frame: &mut Frame, store: &mut Pools, maps: &IndexMaps, instr: &Instr) -> Result<()> {
    let Imm::MemIndex(mi) = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let mi = maps.mem(mi);
    let is64 = store.memories.get(mi).ok_or(Trap::NoMemory)?.is64;
    let delta = frame.pop_mem(is64);
    let byte_ceiling = store.limits.max_memory_bytes; // read before the &mut borrow below
    let mem = &mut store.memories[mi];
    let old_pages = (mem.bytes.len() / PAGE_SIZE) as u64;
    let cap: u64 = if is64 { 0x1_0000_0000_0000 } else { 65536 };
    let limit = mem.max.unwrap_or(cap).min(cap);
    let target = old_pages
        .checked_add(delta)
        .filter(|&p| p <= limit)
        .and_then(|p| usize::try_from(p).ok())
        .and_then(|p| p.checked_mul(PAGE_SIZE))
        .filter(|&n| n <= byte_ceiling);
    match target {
        Some(nbytes) => {
            mem.bytes.resize(nbytes, 0);
            if is64 {
                frame.push_i64(old_pages as i64);
            } else {
                frame.push_i32(old_pages as i32);
            }
        }
        None if is64 => frame.push_i64(-1), // growth refused
        None => frame.push_i32(-1),
    }
    Ok(())
}

fn exec_memory_copy(frame: &mut Frame, store: &mut Pools, maps: &IndexMaps, instr: &Instr) -> Result<()> {
    let Imm::MemCopy { dst, src } = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let (dst, src) = (maps.mem(dst), maps.mem(src));
    let dst64 = store.memories.get(dst).ok_or(Trap::NoMemory)?.is64;
    let src64 = store.memories.get(src).ok_or(Trap::NoMemory)?.is64;
    let n = frame.pop_mem(dst64 && src64);
    let srca = frame.pop_mem(src64);
    let dsta = frame.pop_mem(dst64);
    let si = mem_range(srca, n, store.memories[src].bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let di = mem_range(dsta, n, store.memories[dst].bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let ni = n as usize;
    if dst == src {
        store.memories[dst].bytes.copy_within(si..si + ni, di);
    } else {
        let tmp = store.memories[src].bytes[si..si + ni].to_vec();
        store.memories[dst].bytes[di..di + ni].copy_from_slice(&tmp);
    }
    Ok(())
}

fn exec_memory_fill(frame: &mut Frame, store: &mut Pools, maps: &IndexMaps, instr: &Instr) -> Result<()> {
    let Imm::MemIndex(mi) = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let mi = maps.mem(mi);
    let is64 = store.memories.get(mi).ok_or(Trap::NoMemory)?.is64;
    let n = frame.pop_mem(is64);
    let byte = frame.pop_i32() as u8;
    let dst = frame.pop_mem(is64);
    let mem = &mut store.memories[mi];
    let di = mem_range(dst, n, mem.bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    mem.bytes[di..di + n as usize].fill(byte);
    Ok(())
}

/// The byte width one array element occupies inside a **data segment**.
///
/// Packed fields keep their narrow width; everything else is its natural size. A reference
/// field has no byte form at all, which is why the validator refuses `array.*_data` on one —
/// this returning `None` is the belt to that braces, and it is reachable on the unvalidated
/// run path where a hand-built module can pair any field type with any opcode.
fn data_elem_width(s: StorageType) -> Option<usize> {
    Some(match s {
        StorageType::I8 => 1,
        StorageType::I16 => 2,
        StorageType::Val(v) if v == crate::types::ValType::I32 || v == crate::types::ValType::F32 => 4,
        StorageType::Val(v) if v == crate::types::ValType::I64 || v == crate::types::ValType::F64 => 8,
        StorageType::Val(v) if v == crate::types::ValType::V128 => 16,
        StorageType::Val(_) => return None,
    })
}

/// Read one array element out of a data segment's bytes, little-endian.
fn read_data_elem(s: StorageType, b: &[u8]) -> Value {
    let mut buf = [0u8; 16];
    buf[..b.len()].copy_from_slice(b);
    let raw = u128::from_le_bytes(buf);
    // Packed fields are stored packed; wider ones keep their bits as-is.
    match s {
        StorageType::I8 => raw & 0xff,
        StorageType::I16 => raw & 0xffff,
        StorageType::Val(_) => raw,
    }
}

/// The values of a passive/consumed element segment, as the runtime holds them.
///
/// A dropped segment reads as EMPTY rather than as an error — the same convention `table.init`
/// follows, so an out-of-range access after a drop traps on the bounds check instead of on the
/// drop flag, which is what the spec asks for.
fn elem_segment_values<'a>(ctx: &Ctx, store: &'a Pools, seg: u32) -> Result<&'a [Value]> {
    let ei = ctx.maps.elem(seg);
    let dropped = *store.elem_dropped.get(ei).ok_or(Trap::UndefinedElement)?;
    Ok(if dropped { &[] } else { store.elem_values.get(ei).ok_or(Trap::UndefinedElement)? })
}

/// `array.new_data` / `array.new_elem` — build a fresh array from a segment.
fn exec_array_new_seg(
    frame: &mut Frame,
    ctx: &Ctx,
    store: &mut Pools,
    instr: &Instr,
) -> Result<Value> {
    let Imm::GcTypeSeg { type_index, seg } = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let f = ctx.module.array_field(type_index).ok_or(Trap::UndefinedType)?;
    let n = frame.pop_i32() as u32 as usize;
    let off = frame.pop_i32() as u32 as usize;
    let fields: Vec<Value> = if instr.op == Op::ArrayNewData {
        let dropped = *store
            .data_dropped
            .get(ctx.maps.data(seg))
            .ok_or(Trap::UndefinedData)?;
        let bytes: &[u8] = if dropped {
            &[]
        } else {
            &ctx.module.data.get(seg as usize).ok_or(Trap::UndefinedData)?.bytes
        };
        let w = data_elem_width(f.storage).ok_or(Trap::UnsupportedInstruction)?;
        let end = off
            .checked_add(n.checked_mul(w).ok_or(Trap::MemoryOutOfBounds)?)
            .ok_or(Trap::MemoryOutOfBounds)?;
        if end > bytes.len() {
            return Err(Trap::MemoryOutOfBounds);
        }
        (0..n).map(|k| read_data_elem(f.storage, &bytes[off + k * w..off + (k + 1) * w])).collect()
    } else {
        let vals = elem_segment_values(ctx, store, seg)?;
        let end = off.checked_add(n).ok_or(Trap::TableOutOfBounds)?;
        if end > vals.len() {
            return Err(Trap::TableOutOfBounds);
        }
        vals[off..end].to_vec()
    };
    let r = alloc_object(store, ctx.inst, type_index, fields)?;
    frame.push(r);
    Ok(r)
}

/// `array.init_data` / `array.init_elem` — fill an existing array from a segment.
fn exec_array_init_seg(
    frame: &mut Frame,
    ctx: &Ctx,
    store: &mut Pools,
    instr: &Instr,
) -> Result<()> {
    let Imm::GcTypeSeg { type_index, seg } = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let f = ctx.module.array_field(type_index).ok_or(Trap::UndefinedType)?;
    let n = frame.pop_i32() as u32 as usize;
    let soff = frame.pop_i32() as u32 as usize;
    let doff = frame.pop_i32() as u32 as usize;
    let idx = gc_object_index(store, frame.pop())?;
    let len = store.gc_heap[idx].fields.len();
    if doff.checked_add(n).is_none_or(|e| e > len) {
        return Err(Trap::GcOutOfBounds);
    }
    let run: Vec<Value> = if instr.op == Op::ArrayInitData {
        let dropped = *store
            .data_dropped
            .get(ctx.maps.data(seg))
            .ok_or(Trap::UndefinedData)?;
        let bytes: &[u8] = if dropped {
            &[]
        } else {
            &ctx.module.data.get(seg as usize).ok_or(Trap::UndefinedData)?.bytes
        };
        let w = data_elem_width(f.storage).ok_or(Trap::UnsupportedInstruction)?;
        let end = soff
            .checked_add(n.checked_mul(w).ok_or(Trap::MemoryOutOfBounds)?)
            .ok_or(Trap::MemoryOutOfBounds)?;
        if end > bytes.len() {
            return Err(Trap::MemoryOutOfBounds);
        }
        (0..n).map(|k| read_data_elem(f.storage, &bytes[soff + k * w..soff + (k + 1) * w])).collect()
    } else {
        let vals = elem_segment_values(ctx, store, seg)?;
        let end = soff.checked_add(n).ok_or(Trap::TableOutOfBounds)?;
        if end > vals.len() {
            return Err(Trap::TableOutOfBounds);
        }
        vals[soff..end].to_vec()
    };
    for (k, v) in run.into_iter().enumerate() {
        store.gc_heap[idx].fields[doff + k] = pack_field(f.storage, v);
    }
    Ok(())
}

fn exec_memory_init(frame: &mut Frame, ctx: &Ctx, store: &mut Pools, instr: &Instr) -> Result<()> {
    let Imm::MemInit { data, mem } = instr.imm else {
        return Err(Trap::UnsupportedInstruction);
    };
    let (mi, di) = (ctx.maps.mem(mem), ctx.maps.data(data));
    let is64 = store.memories.get(mi).ok_or(Trap::NoMemory)?.is64;
    let dropped = *store.data_dropped.get(di).ok_or(Trap::UndefinedData)?;
    let empty: &[u8] = &[];
    let seg: &[u8] = if dropped {
        empty
    } else {
        // `di` indexes the store-wide drop flags; the bytes live in this module's own
        // segment list, so they are addressed by the module-local `data`.
        &ctx.module
            .data
            .get(data as usize)
            .ok_or(Trap::UndefinedData)?
            .bytes
    };
    // n/src index the data segment (always i32); dst is a memory address.
    let n = u64::from(frame.pop_i32() as u32);
    let src = u64::from(frame.pop_i32() as u32);
    let dst = frame.pop_mem(is64);
    let si = mem_range(src, n, seg.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let mem = &mut store.memories[mi];
    let di_addr = mem_range(dst, n, mem.bytes.len()).ok_or(Trap::MemoryOutOfBounds)?;
    let ni = n as usize;
    mem.bytes[di_addr..di_addr + ni].copy_from_slice(&seg[si..si + ni]);
    Ok(())
}

/// Interpret a data/element-segment offset constant expression as an address.
fn eval_const_offset(expr: &[u8], globals: &[Value], is64: bool) -> Result<u64> {
    // An offset is an integer, so the owning instance can never matter here — passed as 0 rather
    // than threaded, and the validator refuses a `ref.func` in an offset position regardless.
    let v = eval_const_expr(expr, globals, 0, None)?;
    Ok(if is64 {
        v as u64
    } else {
        u64::from(as_i32(v) as u32)
    })
}

// --- WasmGC helpers ----------------------------------------------------------

/// Narrow a value to a field's storage width before storing (packed i8/i16 keep low bits;
/// an unpacked field — including a `v128`, which fits one 128-bit `Value` — is verbatim).
fn pack_field(storage: StorageType, v: Value) -> Value {
    match storage {
        StorageType::Val(_) => v,
        StorageType::I8 => v & 0xff,
        StorageType::I16 => v & 0xffff,
    }
}

/// Widen a stored field value back to an i32 slot: `_s` sign-extends a packed field, `_u`
/// zero-extends; an unpacked field is verbatim.
fn unpack_field(storage: StorageType, v: Value, signed: bool) -> Value {
    match storage {
        StorageType::Val(_) => v,
        StorageType::I8 if signed => i32_value(i32::from(v as u8 as i8)),
        StorageType::I8 => i32_value(i32::from(v as u8)),
        StorageType::I16 if signed => i32_value(i32::from(v as u16 as i16)),
        StorageType::I16 => i32_value(i32::from(v as u16)),
    }
}

/// The default slot for a field/element of `storage` at `*.new_default` (null for a ref, else 0).
fn default_field(storage: StorageType) -> Value {
    if storage.unpacked().is_ref() {
        NULL_REF
    } else {
        0
    }
}

/// Validate a non-null GC reference and return its heap index, or trap.
fn gc_object_index(store: &Pools, r: Value) -> Result<usize> {
    if r == NULL_REF {
        return Err(Trap::NullReference);
    }
    let idx = usize::try_from(r).map_err(|_| Trap::GcOutOfBounds)?;
    if idx >= store.gc_heap.len() {
        return Err(Trap::GcOutOfBounds);
    }
    Ok(idx)
}

/// A pending **tail call**: the body ended by handing control to another function rather than
/// returning to its own caller.
///
/// ⚠️ This type is the whole point of the tail-call proposal. `return_call f` must **replace** the
/// current frame, not stack a new one on top of it — the feature exists so that mutual recursion can
/// run unbounded, which is exactly what a "call then return" implementation fails to deliver while
/// still producing correct answers on every conformance test. So instead of `run` recursing into
/// `call_function`, it *reports* the intended callee and unwinds; [`call_function`] loops, reusing
/// its own native stack frame. Native stack depth stays constant however long the chain runs.
struct TailCall {
    /// The instance to run in — a tail call can cross a module boundary, and the callee must run
    /// against its own memory and globals like any other cross-instance call.
    inst: usize,
    /// Index in `inst`'s function space. Imports are resolved by `call_function` on the next
    /// iteration, which is also what makes a tail call to a *host* function work without a
    /// special case here.
    func: u32,
    args: Vec<Value>,
}

/// Allocate a GC object, returning its reference value (its heap index).
///
/// `owner` is the allocating instance: `type_index` means nothing without it once the object can be
/// read by another module (see [`HeapObject::owner`]).
fn alloc_object(store: &mut Pools, owner: usize, type_index: u32, fields: Vec<Value>) -> Result<Value> {
    let idx = store.gc_heap.len();
    if idx >= store.limits.max_gc_objects {
        return Err(Trap::GcHeapExhausted);
    }
    store.gc_heap.push(HeapObject {
        owner: owner as u32,
        type_index,
        fields,
    });
    Ok(idx as Value)
}

/// The type index of a *defined* function (for a funcref `ref.cast` to a concrete func type);
/// `None` for an imported function.
fn defined_func_type(module: &Module, fi: u32) -> Option<u32> {
    let imported = module.imported_func_count();
    if fi < imported {
        return None;
    }
    module.functions.get((fi - imported) as usize).copied()
}

/// Match a value's actual heap head against a target heap type — abstract targets use the
/// hierarchy relation, concrete targets the declared subtype chain.
fn head_matches(module: &Module, actual: RefHeap, actual_ti: Option<u32>, target: HeapType) -> bool {
    match target {
        HeapType::Concrete(t) => actual_ti.is_some_and(|ti| module.is_subtype(ti, t)),
        // The uninhabited bottoms: only a null ref has these, already handled by `ref_matches`.
        HeapType::NoFunc | HeapType::NoExtern => false,
        _ => module
            .ref_head(target)
            .is_ok_and(|th| actual.is_subtype_of(th)),
    }
}

/// Does GC reference value `v` match target reference type `rt` (`ref.test`/`ref.cast`)?
///
/// ⚠️ **A reference's TYPE lives in the module that created it, never in the testing one.** `code` and
/// `inst` are what make that resolvable: a funcref carries its owning instance in its bits, and a GC
/// object carries it in [`HeapObject::owner`]. Reading either index against `module` answers a
/// different question than the one asked, and answers it *plausibly* — see the two-directional failure
/// pinned by `tests/gc_cross_module_type_index.wast`.
fn ref_matches(
    module: &Module,
    code: &[InstanceData],
    store: &Pools,
    inst: usize,
    v: Value,
    rt: RefType,
) -> bool {
    if v == NULL_REF {
        return rt.nullable;
    }
    let Ok(target_head) = module.ref_head(rt.heap) else {
        return false;
    };
    match target_head.top() {
        RefHeap::Any => {
            // ⚠️ **The type-confusion guard, and the reason `any.convert_extern` waited for a
            // representation.** Below this point a non-null, non-i31 value is read as a GC heap
            // INDEX. A host reference is not one — it is an opaque address in the embedder's own
            // space — so it must be answered here, before the heap is indexed at all. It is
            // `any` and nothing narrower: `ref_test.wast` index 6 asks `eq` of exactly this value
            // and expects 0.
            if v & HOST_TAG != 0 {
                return target_head == RefHeap::Any;
            }
            // An `externref` wrapper cannot reach an `any` target: validation gives the two
            // disjoint hierarchies. Refusing rather than unwrapping keeps it that way if it ever
            // does — the accept side is the one that reads a field at another type's width.
            if v & EXTERN_TAG != 0 {
                return false;
            }
            if v & I31_TAG != 0 {
                return head_matches(module, RefHeap::I31, None, rt.heap);
            }
            let Ok(idx) = usize::try_from(v) else {
                return false;
            };
            let Some(obj) = store.gc_heap.get(idx) else {
                return false;
            };
            let owner = obj.owner as usize;
            // The object's own module — the only place its `type_index` means anything.
            let owner_module = match code.get(owner) {
                Some(d) => &d.module,
                // No such instance: refuse rather than fall back to `module`, which is precisely the
                // substitution this function exists to prevent.
                None => return false,
            };
            let kind = match owner_module
                .comp_types
                .get(obj.type_index as usize)
                .map(CompType::kind)
            {
                Some(CompKind::Struct) => RefHeap::Struct,
                Some(CompKind::Array) => RefHeap::Array,
                _ => RefHeap::Func,
            };
            // An ABSTRACT target (`any`, `eq`, `struct`, `array`, `i31`, `none`) asks only what shape
            // the object has, which `kind` already answers — no type index is involved, so this is
            // module-independent and needs no registry.
            let HeapType::Concrete(t) = rt.heap else {
                return head_matches(module, kind, None, rt.heap);
            };
            if owner == inst {
                // Same instance: the two indices are in one numbering, so the cheap module-local
                // subtype check is exact. This is the overwhelmingly common case.
                return module.is_subtype(obj.type_index, t);
            }
            // Across a link the two indices are in different modules and cannot be compared directly.
            // Resolve BOTH to store-wide ids and ask the registry — the same mechanism import
            // matching and `call_indirect` use, and the reason wasmrt can answer this exactly where a
            // runtime without a store-wide registry has to settle for a loud false negative.
            match (
                owner_module
                    .comp_types
                    .get(obj.type_index as usize)
                    .and(code[owner].type_ids.get(obj.type_index as usize).copied()),
                code.get(inst)
                    .and_then(|d| d.type_ids.get(t as usize).copied()),
            ) {
                (Some(obj_id), Some(target_id)) => store.types.is_subtype(obj_id, target_id),
                // A hand-built `Module` never joined the registry, so one side has no store-wide id.
                // Refuse. ⚠️ The direction matters and is not a style choice: the ACCEPT side is the
                // dangerous one — a wrongly-accepted `ref.cast` lets the following `struct.get` read a
                // field at another type's width and return a silently wrong value, whereas a wrongly
                // refused cast traps loudly at the cast itself (`best-practices.md`: which direction to
                // err in is a property of the consequence).
                _ => false,
            }
        }
        RefHeap::Func => {
            // Resolve the funcref's type index in the module that DEFINED it — the value carries its
            // owning instance in bits 62..32 precisely so this is possible.
            let owner = funcref_instance(v);
            let ti = code
                .get(owner)
                .and_then(|d| defined_func_type(&d.module, funcref_index(v)));
            // An abstract target needs no index: every non-null funcref is a `func`.
            let HeapType::Concrete(t) = rt.heap else {
                return head_matches(module, RefHeap::Func, ti, rt.heap);
            };
            let Some(ti) = ti else {
                // An IMPORTED function has no defining type index in its own module, so there is
                // nothing to resolve. Refuse, for the same reason as above.
                return false;
            };
            if owner == inst {
                return module.is_subtype(ti, t);
            }
            // ⚠️ This arm used to stop one step short: it fetched `ti` from the owner's module and
            // then compared it against the TESTING module's table anyway — correct about where the
            // index came from, wrong about where it was read. It was logged as "approximate". It is
            // the same defect the GC arm above carried, and the registry decides both exactly.
            match (
                code[owner].type_ids.get(ti as usize).copied(),
                code.get(inst)
                    .and_then(|d| d.type_ids.get(t as usize).copied()),
            ) {
                (Some(got_id), Some(want_id)) => store.types.is_subtype(got_id, want_id),
                _ => false,
            }
        }
        // The `extern` and `exn` hierarchies: every non-null value of one is that hierarchy's
        // top, and neither has a concrete form. ⚠️ Passing the target's OWN top rather than a
        // hardcoded `Extern` is what makes `ref.test exnref` on an `exnref` answer true; the
        // hardcoded version asked whether `extern <: exn` and always said no.
        _ => head_matches(module, target_head.top(), None, rt.heap),
    }
}

/// Integer arithmetic / comparison / bitwise / conversion opcodes. Anything else (float,
/// memory, SIMD, …) traps `UnsupportedInstruction` in this slice.
fn exec_numeric(frame: &mut Frame, op: Op) -> Result<()> {
    match op as u8 {
        // i32 unary
        0x45 => {
            let v = frame.pop_i32();
            frame.push_i32(i32::from(v == 0)); // i32.eqz
        }
        0x67 => {
            let v = frame.pop_i32() as u32;
            frame.push_i32(v.leading_zeros() as i32);
        }
        0x68 => {
            let v = frame.pop_i32() as u32;
            frame.push_i32(v.trailing_zeros() as i32);
        }
        0x69 => {
            let v = frame.pop_i32() as u32;
            frame.push_i32(v.count_ones() as i32);
        }
        // i32 comparison
        0x46..=0x4f => {
            let b = frame.pop_i32();
            let a = frame.pop_i32();
            frame.push_i32(i32::from(cmp_i32(op as u8, a, b)));
        }
        // i32 binary
        0x6a..=0x78 => {
            let b = frame.pop_i32();
            let a = frame.pop_i32();
            frame.push_i32(bin_i32(op as u8, a, b)?);
        }
        0xc0 => {
            let v = frame.pop_i32();
            frame.push_i32(i32::from(v as i8)); // i32.extend8_s
        }
        0xc1 => {
            let v = frame.pop_i32();
            frame.push_i32(i32::from(v as i16)); // i32.extend16_s
        }
        0xa7 => {
            let v = frame.pop_i64();
            frame.push_i32(v as i32); // i32.wrap_i64
        }

        // i64 unary
        0x50 => {
            let v = frame.pop_i64();
            frame.push_i32(i32::from(v == 0)); // i64.eqz (result i32)
        }
        0x79 => {
            let v = frame.pop_i64() as u64;
            frame.push_i64(i64::from(v.leading_zeros()));
        }
        0x7a => {
            let v = frame.pop_i64() as u64;
            frame.push_i64(i64::from(v.trailing_zeros()));
        }
        0x7b => {
            let v = frame.pop_i64() as u64;
            frame.push_i64(i64::from(v.count_ones()));
        }
        // i64 comparison (result i32)
        0x51..=0x5a => {
            let b = frame.pop_i64();
            let a = frame.pop_i64();
            frame.push_i32(i32::from(cmp_i64(op as u8, a, b)));
        }
        // i64 binary
        0x7c..=0x8a => {
            let b = frame.pop_i64();
            let a = frame.pop_i64();
            frame.push_i64(bin_i64(op as u8, a, b)?);
        }
        0xc2 => {
            let v = frame.pop_i64();
            frame.push_i64(i64::from(v as i8)); // i64.extend8_s
        }
        0xc3 => {
            let v = frame.pop_i64();
            frame.push_i64(i64::from(v as i16)); // i64.extend16_s
        }
        0xc4 => {
            let v = frame.pop_i64();
            frame.push_i64(i64::from(v as i32)); // i64.extend32_s
        }
        0xac => {
            let v = frame.pop_i32();
            frame.push_i64(i64::from(v)); // i64.extend_i32_s
        }
        0xad => {
            let v = frame.pop_i32();
            frame.push_i64(i64::from(v as u32)); // i64.extend_i32_u
        }

        _ => return exec_float(frame, op),
    }
    Ok(())
}

/// Float arithmetic / comparison / conversion opcodes (IEEE 754). Rounding ops use bit
/// manipulation (no_std-clean); `sqrt` is gated behind `std`. Saturating float→int uses
/// Rust's `as` cast, which matches wasm exactly (NaN→0, saturates to min/max).
fn exec_float(frame: &mut Frame, op: Op) -> Result<()> {
    match op as u8 {
        // f32 / f64 comparison (result i32) — IEEE ordering; NaN compares false (ne true).
        0x5b..=0x60 => {
            let b = frame.pop_f32();
            let a = frame.pop_f32();
            frame.push_i32(i32::from(fcmp(op as u8 - 0x5b, f64::from(a), f64::from(b))));
        }
        0x61..=0x66 => {
            let b = frame.pop_f64();
            let a = frame.pop_f64();
            frame.push_i32(i32::from(fcmp(op as u8 - 0x61, a, b)));
        }

        // f32 unary
        0x8b => {
            let v = frame.pop_f32();
            frame.push_f32(fabs_f32(v));
        }
        0x8c => {
            let v = frame.pop_f32();
            frame.push_f32(-v);
        }
        0x8d => {
            let v = frame.pop_f32();
            frame.push_f32(ceil_f32(v));
        }
        0x8e => {
            let v = frame.pop_f32();
            frame.push_f32(floor_f32(v));
        }
        0x8f => {
            let v = frame.pop_f32();
            frame.push_f32(trunc_f32(v));
        }
        0x90 => {
            let v = frame.pop_f32();
            frame.push_f32(nearest_f32(v));
        }
        0x91 => {
            let v = frame.pop_f32();
            frame.push_f32(sqrt_f32(v)?);
        }
        // f32 binary
        0x92..=0x98 => {
            let b = frame.pop_f32();
            let a = frame.pop_f32();
            frame.push_f32(fbin_f32(op as u8, a, b));
        }

        // f64 unary
        0x99 => {
            let v = frame.pop_f64();
            frame.push_f64(fabs_f64(v));
        }
        0x9a => {
            let v = frame.pop_f64();
            frame.push_f64(-v);
        }
        0x9b => {
            let v = frame.pop_f64();
            frame.push_f64(ceil_f64(v));
        }
        0x9c => {
            let v = frame.pop_f64();
            frame.push_f64(floor_f64(v));
        }
        0x9d => {
            let v = frame.pop_f64();
            frame.push_f64(trunc_f64(v));
        }
        0x9e => {
            let v = frame.pop_f64();
            frame.push_f64(nearest_f64(v));
        }
        0x9f => {
            let v = frame.pop_f64();
            frame.push_f64(sqrt_f64(v)?);
        }
        // f64 binary
        0xa0..=0xa6 => {
            let b = frame.pop_f64();
            let a = frame.pop_f64();
            frame.push_f64(fbin_f64(op as u8, a, b));
        }

        // Float → int, trapping.
        0xa8 => {
            let t = trap_trunc_f32(frame.pop_f32(), -2_147_483_648.0, 2_147_483_648.0)?;
            frame.push_i32(t as i32);
        }
        0xa9 => {
            let t = trap_trunc_f32(frame.pop_f32(), 0.0, 4_294_967_296.0)?;
            frame.push_i32(t as u32 as i32);
        }
        0xaa => {
            let t = trap_trunc_f64(frame.pop_f64(), -2_147_483_648.0, 2_147_483_648.0)?;
            frame.push_i32(t as i32);
        }
        0xab => {
            let t = trap_trunc_f64(frame.pop_f64(), 0.0, 4_294_967_296.0)?;
            frame.push_i32(t as u32 as i32);
        }
        0xae => {
            let t = trap_trunc_f32(frame.pop_f32(), -9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0)?;
            frame.push_i64(t as i64);
        }
        0xaf => {
            let t = trap_trunc_f32(frame.pop_f32(), 0.0, 18_446_744_073_709_551_616.0)?;
            frame.push_i64(t as u64 as i64);
        }
        0xb0 => {
            let t = trap_trunc_f64(frame.pop_f64(), -9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0)?;
            frame.push_i64(t as i64);
        }
        0xb1 => {
            let t = trap_trunc_f64(frame.pop_f64(), 0.0, 18_446_744_073_709_551_616.0)?;
            frame.push_i64(t as u64 as i64);
        }

        // Int → float
        0xb2 => {
            let v = frame.pop_i32();
            frame.push_f32(v as f32);
        }
        0xb3 => {
            let v = frame.pop_i32();
            frame.push_f32(v as u32 as f32);
        }
        0xb4 => {
            let v = frame.pop_i64();
            frame.push_f32(v as f32);
        }
        0xb5 => {
            let v = frame.pop_i64();
            frame.push_f32(v as u64 as f32);
        }
        0xb6 => {
            let v = frame.pop_f64();
            frame.push_f32(v as f32); // f32.demote_f64
        }
        0xb7 => {
            let v = frame.pop_i32();
            frame.push_f64(f64::from(v));
        }
        0xb8 => {
            let v = frame.pop_i32();
            frame.push_f64(f64::from(v as u32));
        }
        0xb9 => {
            let v = frame.pop_i64();
            frame.push_f64(v as f64);
        }
        0xba => {
            let v = frame.pop_i64();
            frame.push_f64(v as u64 as f64);
        }
        0xbb => {
            let v = frame.pop_f32();
            frame.push_f64(f64::from(v)); // f64.promote_f32
        }

        // Reinterpret: the u64 slot already holds the bit pattern, so these are identity.
        0xbc..=0xbf => {}

        // Float → int, saturating (Rust's `as` matches wasm: NaN→0, saturates).
        0xc5 => {
            let v = frame.pop_f32();
            frame.push_i32(v as i32);
        }
        0xc6 => {
            let v = frame.pop_f32();
            frame.push_i32(v as u32 as i32);
        }
        0xc7 => {
            let v = frame.pop_f64();
            frame.push_i32(v as i32);
        }
        0xc8 => {
            let v = frame.pop_f64();
            frame.push_i32(v as u32 as i32);
        }
        0xc9 => {
            let v = frame.pop_f32();
            frame.push_i64(v as i64);
        }
        0xca => {
            let v = frame.pop_f32();
            frame.push_i64(v as u64 as i64);
        }
        0xcb => {
            let v = frame.pop_f64();
            frame.push_i64(v as i64);
        }
        0xcc => {
            let v = frame.pop_f64();
            frame.push_i64(v as u64 as i64);
        }

        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

/// Float comparison, keyed by the offset within its 6-op group (eq/ne/lt/gt/le/ge). Done in
/// f64 (an f32 comparison widens losslessly and orders identically).
fn fcmp(rel: u8, a: f64, b: f64) -> bool {
    match rel {
        0 => a == b,
        1 => a != b,
        2 => a < b,
        3 => a > b,
        4 => a <= b,
        _ => a >= b, // ge
    }
}

fn fbin_f32(op: u8, a: f32, b: f32) -> f32 {
    match op {
        0x92 => a + b,
        0x93 => a - b,
        0x94 => a * b,
        0x95 => a / b,
        0x96 => fmin_f32(a, b),
        0x97 => fmax_f32(a, b),
        _ => fcopysign_f32(a, b), // 0x98 copysign
    }
}
fn fbin_f64(op: u8, a: f64, b: f64) -> f64 {
    match op {
        0xa0 => a + b,
        0xa1 => a - b,
        0xa2 => a * b,
        0xa3 => a / b,
        0xa4 => fmin_f64(a, b),
        0xa5 => fmax_f64(a, b),
        _ => fcopysign_f64(a, b), // 0xa6 copysign
    }
}

// --- Float helpers (bit-manipulation; no_std-clean) --------------------------

fn fabs_f32(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7fff_ffff)
}
fn fabs_f64(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & 0x7fff_ffff_ffff_ffff)
}
fn fcopysign_f32(x: f32, y: f32) -> f32 {
    f32::from_bits((x.to_bits() & 0x7fff_ffff) | (y.to_bits() & 0x8000_0000))
}
fn fcopysign_f64(x: f64, y: f64) -> f64 {
    f64::from_bits((x.to_bits() & 0x7fff_ffff_ffff_ffff) | (y.to_bits() & 0x8000_0000_0000_0000))
}

/// wasm `fmin`: NaN-propagating, and `min(+0,-0) == -0` (sign-bit OR).
fn fmin_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a < b {
        a
    } else if b < a {
        b
    } else {
        f32::from_bits(a.to_bits() | b.to_bits())
    }
}
fn fmin_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a < b {
        a
    } else if b < a {
        b
    } else {
        f64::from_bits(a.to_bits() | b.to_bits())
    }
}
/// wasm `fmax`: NaN-propagating, and `max(+0,-0) == +0` (sign-bit AND).
fn fmax_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a > b {
        a
    } else if b > a {
        b
    } else {
        f32::from_bits(a.to_bits() & b.to_bits())
    }
}
fn fmax_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a > b {
        a
    } else if b > a {
        b
    } else {
        f64::from_bits(a.to_bits() & b.to_bits())
    }
}

fn trunc_f32(x: f32) -> f32 {
    // ⚠⚠ NaN propagation must produce an **arithmetic** NaN — the quiet bit set — so a
    // SIGNALLING input is quieted rather than passed through. `nearest_f32` already did this;
    // `trunc` did not, and `floor`/`ceil` delegate here, so **three of the four rounding ops**
    // returned the input NaN unchanged. Worth 28 assertions across `f32.wast`, `f64.wast` and
    // both `simd_*_rounding.wast` files. *The rule was written once and applied at one of four
    // sites* — fixing it HERE is what makes floor and ceil inherit it.
    if x.is_nan() {
        return f32::from_bits(x.to_bits() | 0x0040_0000);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127;
    if exp < 0 {
        return f32::from_bits(bits & 0x8000_0000); // |x| < 1 → ±0
    }
    if exp >= 23 {
        return x; // already integer, or inf/nan
    }
    let mask = (1u32 << (23 - exp)) - 1;
    f32::from_bits(bits & !mask)
}
fn trunc_f64(x: f64) -> f64 {
    // See `trunc_f32`: a signalling NaN is quieted, and `floor_f64`/`ceil_f64` inherit it.
    if x.is_nan() {
        return f64::from_bits(x.to_bits() | 0x0008_0000_0000_0000);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1023;
    if exp < 0 {
        return f64::from_bits(bits & 0x8000_0000_0000_0000);
    }
    if exp >= 52 {
        return x;
    }
    let mask = (1u64 << (52 - exp)) - 1;
    f64::from_bits(bits & !mask)
}
fn floor_f32(x: f32) -> f32 {
    let t = trunc_f32(x);
    if x < 0.0 && x != t {
        t - 1.0
    } else {
        t
    }
}
fn floor_f64(x: f64) -> f64 {
    let t = trunc_f64(x);
    if x < 0.0 && x != t {
        t - 1.0
    } else {
        t
    }
}
fn ceil_f32(x: f32) -> f32 {
    let t = trunc_f32(x);
    if x > 0.0 && x != t {
        t + 1.0
    } else {
        t
    }
}
fn ceil_f64(x: f64) -> f64 {
    let t = trunc_f64(x);
    if x > 0.0 && x != t {
        t + 1.0
    } else {
        t
    }
}
/// wasm `nearest`: round to nearest, ties to even, preserving the sign of zero and quieting
/// a NaN operand.
fn nearest_f32(x: f32) -> f32 {
    if x.is_nan() {
        return f32::from_bits(x.to_bits() | 0x0040_0000); // set the quiet bit
    }
    if !x.is_finite() {
        return x;
    }
    let f = floor_f32(x);
    let diff = x - f;
    // Round up when past the halfway point, or exactly halfway with an odd floor (ties→even).
    let r = if diff > 0.5 || (diff == 0.5 && f % 2.0 != 0.0) {
        f + 1.0
    } else {
        f
    };
    if r == 0.0 {
        return fcopysign_f32(0.0, x); // a rounded-to-zero result keeps x's sign
    }
    r
}
fn nearest_f64(x: f64) -> f64 {
    if x.is_nan() {
        return f64::from_bits(x.to_bits() | 0x0008_0000_0000_0000);
    }
    if !x.is_finite() {
        return x;
    }
    let f = floor_f64(x);
    let diff = x - f;
    let r = if diff > 0.5 || (diff == 0.5 && f % 2.0 != 0.0) {
        f + 1.0
    } else {
        f
    };
    if r == 0.0 {
        return fcopysign_f64(0.0, x); // a rounded-to-zero result keeps x's sign
    }
    r
}

/// Trapping float→int range check: NaN or `trunc(x) ∉ [lo, hi)` traps; else returns
/// `trunc(x)` for the caller to cast (in range, the cast is exact).
fn trap_trunc_f32(x: f32, lo: f32, hi: f32) -> Result<f32> {
    if x.is_nan() {
        return Err(Trap::InvalidConversionToInt);
    }
    let t = trunc_f32(x);
    if t < lo || t >= hi {
        return Err(Trap::InvalidConversionToInt);
    }
    Ok(t)
}
fn trap_trunc_f64(x: f64, lo: f64, hi: f64) -> Result<f64> {
    if x.is_nan() {
        return Err(Trap::InvalidConversionToInt);
    }
    let t = trunc_f64(x);
    if t < lo || t >= hi {
        return Err(Trap::InvalidConversionToInt);
    }
    Ok(t)
}

/// `f32.sqrt` / `f64.sqrt`. Correctly-rounded sqrt needs the platform math library, so it is
/// available with the `std` feature (the default); a freestanding `no_std` build traps until
/// a software sqrt lands.
#[cfg(feature = "std")]
fn sqrt_f32(x: f32) -> Result<f32> {
    Ok(x.sqrt())
}
#[cfg(feature = "std")]
fn sqrt_f64(x: f64) -> Result<f64> {
    Ok(x.sqrt())
}
#[cfg(not(feature = "std"))]
fn sqrt_f32(_x: f32) -> Result<f32> {
    Err(Trap::UnsupportedInstruction)
}
#[cfg(not(feature = "std"))]
fn sqrt_f64(_x: f64) -> Result<f64> {
    Err(Trap::UnsupportedInstruction)
}

// ============================ SIMD (v128, 0xFD) =============================
//
// A `v128` is a single 128-bit `Value` slot, so `frame.push`/`frame.pop` move it
// with no special-casing. These `v_*`/`p_*` helpers view a `Value` as a
// little-endian lane array and back — endian-explicit (`to_le_bytes`), so the
// wasm SIMD little-endian lane order holds on any host. Ported opcode-for-opcode
// from wazmrt `interp.zig` `execSimd` (frozen oracle @dadc727).

macro_rules! lane_views {
    ($unpack:ident, $pack:ident, $t:ty, $n:expr, $sz:expr) => {
        #[inline]
        fn $unpack(v: Value) -> [$t; $n] {
            let b = v.to_le_bytes();
            core::array::from_fn(|i| <$t>::from_le_bytes(b[i * $sz..i * $sz + $sz].try_into().unwrap()))
        }
        #[inline]
        fn $pack(a: [$t; $n]) -> Value {
            let mut b = [0u8; 16];
            for (i, x) in a.iter().enumerate() {
                b[i * $sz..i * $sz + $sz].copy_from_slice(&x.to_le_bytes());
            }
            Value::from_le_bytes(b)
        }
    };
}
lane_views!(v_u8x16, p_u8x16, u8, 16, 1);
lane_views!(v_i8x16, p_i8x16, i8, 16, 1);
lane_views!(v_u16x8, p_u16x8, u16, 8, 2);
lane_views!(v_i16x8, p_i16x8, i16, 8, 2);
lane_views!(v_u32x4, p_u32x4, u32, 4, 4);
lane_views!(v_i32x4, p_i32x4, i32, 4, 4);
lane_views!(v_u64x2, p_u64x2, u64, 2, 8);
lane_views!(v_i64x2, p_i64x2, i64, 2, 8);
lane_views!(v_f32x4, p_f32x4, f32, 4, 4);
lane_views!(v_f64x2, p_f64x2, f64, 2, 8);

/// Saturating float→int truncation for one lane (NaN→0), bounds compared in f64.
fn sat_trunc_i32(x: f64) -> i32 {
    if x.is_nan() {
        return 0;
    }
    let t = trunc_f64(x);
    if t <= i32::MIN as f64 {
        return i32::MIN;
    }
    if t >= i32::MAX as f64 {
        return i32::MAX;
    }
    t as i32
}
fn sat_trunc_u32(x: f64) -> u32 {
    if x.is_nan() {
        return 0;
    }
    let t = trunc_f64(x);
    if t <= 0.0 {
        return 0;
    }
    if t >= u32::MAX as f64 {
        return u32::MAX;
    }
    t as u32
}
#[inline]
fn fneg_f32(x: f32) -> f32 {
    f32::from_bits(x.to_bits() ^ 0x8000_0000)
}
#[inline]
fn fneg_f64(x: f64) -> f64 {
    f64::from_bits(x.to_bits() ^ 0x8000_0000_0000_0000)
}

/// Pop an address and bounds-check `n` bytes for a SIMD memory op; returns the
/// effective byte offset into memory `ma.memory` (memory64-aware, overflow-safe).
fn simd_mem_ea(frame: &mut Frame, store: &Pools, maps: &IndexMaps, ma: crate::opcode::MemArg, n: u64) -> Result<usize> {
    let mem = store.memories.get(maps.mem(ma.memory)).ok_or(Trap::NoMemory)?;
    let addr = frame.pop_mem(mem.is64);
    let ea = addr.checked_add(ma.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(n).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    Ok(ea as usize)
}

// Lane-wise op macros (the comptime-helper analogs). `$f` is the frame.
macro_rules! simd_bin {
    ($f:expr, $up:ident, $pk:ident, |$a:ident, $b:ident| $body:expr) => {{
        let bb = $up($f.pop());
        let aa = $up($f.pop());
        let mut r = aa;
        for (i, slot) in r.iter_mut().enumerate() {
            let $a = aa[i];
            let $b = bb[i];
            *slot = $body;
        }
        $f.push($pk(r));
    }};
}
macro_rules! simd_un {
    ($f:expr, $up:ident, $pk:ident, |$x:ident| $body:expr) => {{
        let aa = $up($f.pop());
        let mut r = aa;
        for (i, slot) in r.iter_mut().enumerate() {
            let $x = aa[i];
            *slot = $body;
        }
        $f.push($pk(r));
    }};
}
macro_rules! simd_cmp {
    ($f:expr, $up:ident, $pk:ident, $ures:ty, |$a:ident, $b:ident| $body:expr) => {{
        let bb = $up($f.pop());
        let aa = $up($f.pop());
        $f.push($pk(core::array::from_fn(|i| {
            let $a = aa[i];
            let $b = bb[i];
            if $body { <$ures>::MAX } else { 0 }
        })));
    }};
}
macro_rules! simd_shift {
    ($f:expr, $up:ident, $pk:ident, $bits:expr, $op:tt) => {{
        let amt = ($f.pop_i32() as u32) % $bits;
        let aa = $up($f.pop());
        $f.push($pk(aa.map(|x| x $op amt)));
    }};
}
macro_rules! simd_extend {
    ($f:expr, $up:ident, $pk:ident, $dst:ty, $half:expr, $high:expr) => {{
        let src = $up($f.pop());
        let base: usize = if $high { $half } else { 0 };
        $f.push($pk(core::array::from_fn(|i| src[base + i] as $dst)));
    }};
}
macro_rules! simd_narrow {
    ($f:expr, $up:ident, $pk:ident, $src:ty, $dst:ty) => {{
        let b = $up($f.pop());
        let a = $up($f.pop());
        let half = a.len();
        $f.push($pk(core::array::from_fn(|i| {
            let x: $src = if i < half { a[i] } else { b[i - half] };
            x.clamp(<$dst>::MIN as $src, <$dst>::MAX as $src) as $dst
        })));
    }};
}
macro_rules! simd_convert {
    ($f:expr, $up:ident, $pk:ident, $dst:ty) => {{
        let src = $up($f.pop());
        $f.push($pk(core::array::from_fn(|i| src[i] as $dst)));
    }};
}
macro_rules! simd_extmul {
    ($f:expr, $up:ident, $pk:ident, $dst:ty, $n:expr, $high:expr) => {{
        let b = $up($f.pop());
        let a = $up($f.pop());
        let base: usize = if $high { $n } else { 0 };
        $f.push($pk(core::array::from_fn(|i| (a[base + i] as $dst) * (b[base + i] as $dst))));
    }};
}
macro_rules! simd_extadd {
    ($f:expr, $up:ident, $pk:ident, $dst:ty) => {{
        let src = $up($f.pop());
        $f.push($pk(core::array::from_fn(|i| src[2 * i] as $dst + src[2 * i + 1] as $dst)));
    }};
}
macro_rules! simd_load_extend {
    ($f:expr, $store:expr, $maps:expr, $mem:expr, $srcty:ty, $srcsz:expr, $n:expr, $pk:ident, $dst:ty) => {{
        let ea = simd_mem_ea($f, $store, $maps, $mem, 8)?;
        let m = &$store.memories[$maps.mem($mem.memory)];
        let src: [$srcty; $n] = core::array::from_fn(|i| {
            <$srcty>::from_le_bytes(m.bytes[ea + i * $srcsz..ea + i * $srcsz + $srcsz].try_into().unwrap())
        });
        $f.push($pk(core::array::from_fn(|i| src[i] as $dst)));
    }};
}

/// Execute a `0xFD` SIMD instruction. Covers the entire fixed-width + relaxed SIMD
/// set; an unknown sub-opcode traps `UnsupportedInstruction`.
#[allow(clippy::too_many_lines)]
fn exec_simd(frame: &mut Frame, store: &mut Pools, maps: &IndexMaps, s: crate::opcode::Simd) -> Result<()> {
    let lane = s.lane as usize;
    match s.sub {
        // --- const / load / store ---
        0x0c => frame.push(s.bytes), // v128.const
        0x00 => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 16)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            frame.push(u128::from_le_bytes(m.bytes[ea..ea + 16].try_into().unwrap()));
        }
        0x0b => {
            let v = frame.pop();
            let ea = simd_mem_ea(frame, store, maps, s.mem, 16)?;
            store.memories[maps.mem(s.mem.memory)].bytes[ea..ea + 16].copy_from_slice(&v.to_le_bytes());
        }
        // --- shuffle / swizzle ---
        0x0d => {
            let b = v_u8x16(frame.pop());
            let a = v_u8x16(frame.pop());
            let idx = s.bytes.to_le_bytes();
            frame.push(p_u8x16(core::array::from_fn(|i| {
                let j = idx[i] as usize;
                if j < 16 {
                    a[j]
                } else if j < 32 {
                    b[j - 16]
                } else {
                    0
                }
            })));
        }
        0x0e | 0x100 => {
            let idx = v_u8x16(frame.pop());
            let a = v_u8x16(frame.pop());
            frame.push(p_u8x16(core::array::from_fn(|i| {
                let j = idx[i] as usize;
                if j < 16 { a[j] } else { 0 }
            })));
        }
        // --- splat ---
        0x0f => {
            let x = frame.pop_i32() as u8;
            frame.push(p_u8x16([x; 16]));
        }
        0x10 => {
            let x = frame.pop_i32() as u16;
            frame.push(p_u16x8([x; 8]));
        }
        0x11 => {
            let x = frame.pop_i32() as u32;
            frame.push(p_u32x4([x; 4]));
        }
        0x12 => {
            let x = frame.pop_i64() as u64;
            frame.push(p_u64x2([x; 2]));
        }
        0x13 => {
            let x = frame.pop() as u32;
            frame.push(p_u32x4([x; 4]));
        }
        0x14 => {
            let x = frame.pop() as u64;
            frame.push(p_u64x2([x; 2]));
        }
        // --- extract_lane ---
        0x15 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_i8x16(v)[lane]));
        }
        0x16 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u8x16(v)[lane]));
        }
        0x18 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_i16x8(v)[lane]));
        }
        0x19 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u16x8(v)[lane]));
        }
        0x1b => {
            let v = frame.pop();
            frame.push_i32(v_i32x4(v)[lane]);
        }
        0x1d => {
            let v = frame.pop();
            frame.push(Value::from(v_u64x2(v)[lane]));
        }
        0x1f => {
            let v = frame.pop();
            frame.push(Value::from(v_u32x4(v)[lane]));
        }
        0x21 => {
            let v = frame.pop();
            frame.push(Value::from(v_u64x2(v)[lane]));
        }
        // --- replace_lane ---
        0x17 => {
            let x = frame.pop_i32() as u8;
            let mut a = v_u8x16(frame.pop());
            a[lane] = x;
            frame.push(p_u8x16(a));
        }
        0x1a => {
            let x = frame.pop_i32() as u16;
            let mut a = v_u16x8(frame.pop());
            a[lane] = x;
            frame.push(p_u16x8(a));
        }
        0x1c => {
            let x = frame.pop_i32() as u32;
            let mut a = v_u32x4(frame.pop());
            a[lane] = x;
            frame.push(p_u32x4(a));
        }
        0x1e => {
            let x = frame.pop_i64() as u64;
            let mut a = v_u64x2(frame.pop());
            a[lane] = x;
            frame.push(p_u64x2(a));
        }
        0x20 => {
            let x = frame.pop() as u32;
            let mut a = v_u32x4(frame.pop());
            a[lane] = x;
            frame.push(p_u32x4(a));
        }
        0x22 => {
            let x = frame.pop() as u64;
            let mut a = v_u64x2(frame.pop());
            a[lane] = x;
            frame.push(p_u64x2(a));
        }
        // --- comparisons ---
        0x23 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a == b),
        0x24 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a != b),
        0x25 => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a < b),
        0x26 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a < b),
        0x27 => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a > b),
        0x28 => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a > b),
        0x29 => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a <= b),
        0x2a => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a <= b),
        0x2b => simd_cmp!(frame, v_i8x16, p_u8x16, u8, |a, b| a >= b),
        0x2c => simd_cmp!(frame, v_u8x16, p_u8x16, u8, |a, b| a >= b),
        0x2d => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a == b),
        0x2e => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a != b),
        0x2f => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a < b),
        0x30 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a < b),
        0x31 => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a > b),
        0x32 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a > b),
        0x33 => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a <= b),
        0x34 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a <= b),
        0x35 => simd_cmp!(frame, v_i16x8, p_u16x8, u16, |a, b| a >= b),
        0x36 => simd_cmp!(frame, v_u16x8, p_u16x8, u16, |a, b| a >= b),
        0x37 => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a == b),
        0x38 => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a != b),
        0x39 => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a < b),
        0x3a => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a < b),
        0x3b => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a > b),
        0x3c => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a > b),
        0x3d => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a <= b),
        0x3e => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a <= b),
        0x3f => simd_cmp!(frame, v_i32x4, p_u32x4, u32, |a, b| a >= b),
        0x40 => simd_cmp!(frame, v_u32x4, p_u32x4, u32, |a, b| a >= b),
        0x41 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a == b),
        0x42 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a != b),
        0x43 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a < b),
        0x44 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a > b),
        0x45 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a <= b),
        0x46 => simd_cmp!(frame, v_f32x4, p_u32x4, u32, |a, b| a >= b),
        0x47 => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a == b),
        0x48 => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a != b),
        0x49 => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a < b),
        0x4a => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a > b),
        0x4b => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a <= b),
        0x4c => simd_cmp!(frame, v_f64x2, p_u64x2, u64, |a, b| a >= b),
        0xd6 => simd_cmp!(frame, v_u64x2, p_u64x2, u64, |a, b| a == b),
        0xd7 => simd_cmp!(frame, v_u64x2, p_u64x2, u64, |a, b| a != b),
        0xd8 => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a < b),
        0xd9 => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a > b),
        0xda => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a <= b),
        0xdb => simd_cmp!(frame, v_i64x2, p_u64x2, u64, |a, b| a >= b),
        // --- bitwise (whole register) ---
        0x4d => {
            let v = frame.pop();
            frame.push(!v);
        }
        0x4e => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a & b);
        }
        0x4f => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a & !b);
        }
        0x50 => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a | b);
        }
        0x51 => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(a ^ b);
        }
        0x52 => {
            let c = frame.pop();
            let b = frame.pop();
            let a = frame.pop();
            frame.push((a & c) | (b & !c));
        }
        0x53 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v != 0)); // v128.any_true
        }
        // --- i8x16 ---
        0x60 => simd_un!(frame, v_i8x16, p_i8x16, |x| x.wrapping_abs()),
        0x61 => simd_un!(frame, v_i8x16, p_i8x16, |x| x.wrapping_neg()),
        0x62 => simd_un!(frame, v_u8x16, p_u8x16, |x| x.count_ones() as u8),
        0x63 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u8x16(v).iter().all(|&x| x != 0)));
        }
        0x64 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u8x16(v), 8));
        }
        0x6b => simd_shift!(frame, v_u8x16, p_u8x16, 8, <<),
        0x6c => simd_shift!(frame, v_i8x16, p_i8x16, 8, >>),
        0x6d => simd_shift!(frame, v_u8x16, p_u8x16, 8, >>),
        0x6e => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.wrapping_add(b)),
        0x6f => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.saturating_add(b)),
        0x70 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.saturating_add(b)),
        0x71 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.wrapping_sub(b)),
        0x72 => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.saturating_sub(b)),
        0x73 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.saturating_sub(b)),
        0x76 => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.min(b)),
        0x77 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.min(b)),
        0x78 => simd_bin!(frame, v_i8x16, p_i8x16, |a, b| a.max(b)),
        0x79 => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| a.max(b)),
        0x7b => simd_bin!(frame, v_u8x16, p_u8x16, |a, b| ((u16::from(a) + u16::from(b) + 1) >> 1) as u8),
        0x7c => simd_extadd!(frame, v_i8x16, p_i16x8, i16),
        0x7d => simd_extadd!(frame, v_u8x16, p_u16x8, u16),
        0x7e => simd_extadd!(frame, v_i16x8, p_i32x4, i32),
        0x7f => simd_extadd!(frame, v_u16x8, p_u32x4, u32),
        // --- i16x8 ---
        0x80 => simd_un!(frame, v_i16x8, p_i16x8, |x| x.wrapping_abs()),
        0x81 => simd_un!(frame, v_i16x8, p_i16x8, |x| x.wrapping_neg()),
        0x82 => {
            let b = v_i16x8(frame.pop());
            let a = v_i16x8(frame.pop());
            frame.push(p_i16x8(core::array::from_fn(|i| {
                let p = (i32::from(a[i]) * i32::from(b[i]) + 0x4000) >> 15;
                p.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })));
        }
        0x83 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u16x8(v).iter().all(|&x| x != 0)));
        }
        0x84 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u16x8(v), 16));
        }
        0x85 => simd_narrow!(frame, v_i32x4, p_i16x8, i32, i16),
        0x86 => simd_narrow!(frame, v_i32x4, p_u16x8, i32, u16),
        0x87 => simd_extend!(frame, v_i8x16, p_i16x8, i16, 8, false),
        0x88 => simd_extend!(frame, v_i8x16, p_i16x8, i16, 8, true),
        0x89 => simd_extend!(frame, v_u8x16, p_u16x8, u16, 8, false),
        0x8a => simd_extend!(frame, v_u8x16, p_u16x8, u16, 8, true),
        0x8b => simd_shift!(frame, v_u16x8, p_u16x8, 16, <<),
        0x8c => simd_shift!(frame, v_i16x8, p_i16x8, 16, >>),
        0x8d => simd_shift!(frame, v_u16x8, p_u16x8, 16, >>),
        0x8e => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.wrapping_add(b)),
        0x8f => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.saturating_add(b)),
        0x90 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.saturating_add(b)),
        0x91 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.wrapping_sub(b)),
        0x92 => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.saturating_sub(b)),
        0x93 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.saturating_sub(b)),
        0x95 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.wrapping_mul(b)),
        0x96 => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.min(b)),
        0x97 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.min(b)),
        0x98 => simd_bin!(frame, v_i16x8, p_i16x8, |a, b| a.max(b)),
        0x99 => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| a.max(b)),
        0x9b => simd_bin!(frame, v_u16x8, p_u16x8, |a, b| ((u32::from(a) + u32::from(b) + 1) >> 1) as u16),
        0x9c => simd_extmul!(frame, v_i8x16, p_i16x8, i16, 8, false),
        0x9d => simd_extmul!(frame, v_i8x16, p_i16x8, i16, 8, true),
        0x9e => simd_extmul!(frame, v_u8x16, p_u16x8, u16, 8, false),
        0x9f => simd_extmul!(frame, v_u8x16, p_u16x8, u16, 8, true),
        // --- i32x4 ---
        0xa0 => simd_un!(frame, v_i32x4, p_i32x4, |x| x.wrapping_abs()),
        0xa1 => simd_un!(frame, v_i32x4, p_i32x4, |x| x.wrapping_neg()),
        0xa3 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u32x4(v).iter().all(|&x| x != 0)));
        }
        0xa4 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u32x4(v), 32));
        }
        0xa7 => simd_extend!(frame, v_i16x8, p_i32x4, i32, 4, false),
        0xa8 => simd_extend!(frame, v_i16x8, p_i32x4, i32, 4, true),
        0xa9 => simd_extend!(frame, v_u16x8, p_u32x4, u32, 4, false),
        0xaa => simd_extend!(frame, v_u16x8, p_u32x4, u32, 4, true),
        0xab => simd_shift!(frame, v_u32x4, p_u32x4, 32, <<),
        0xac => simd_shift!(frame, v_i32x4, p_i32x4, 32, >>),
        0xad => simd_shift!(frame, v_u32x4, p_u32x4, 32, >>),
        0xae => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.wrapping_add(b)),
        0xb1 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.wrapping_sub(b)),
        0xb5 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.wrapping_mul(b)),
        0xb6 => simd_bin!(frame, v_i32x4, p_i32x4, |a, b| a.min(b)),
        0xb7 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.min(b)),
        0xb8 => simd_bin!(frame, v_i32x4, p_i32x4, |a, b| a.max(b)),
        0xb9 => simd_bin!(frame, v_u32x4, p_u32x4, |a, b| a.max(b)),
        0xba => {
            let b = v_i16x8(frame.pop());
            let a = v_i16x8(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| {
                (i32::from(a[2 * i]) * i32::from(b[2 * i]))
                    .wrapping_add(i32::from(a[2 * i + 1]) * i32::from(b[2 * i + 1]))
            })));
        }
        0xbc => simd_extmul!(frame, v_i16x8, p_i32x4, i32, 4, false),
        0xbd => simd_extmul!(frame, v_i16x8, p_i32x4, i32, 4, true),
        0xbe => simd_extmul!(frame, v_u16x8, p_u32x4, u32, 4, false),
        0xbf => simd_extmul!(frame, v_u16x8, p_u32x4, u32, 4, true),
        // --- i64x2 ---
        0xc0 => simd_un!(frame, v_i64x2, p_i64x2, |x| x.wrapping_abs()),
        0xc1 => simd_un!(frame, v_i64x2, p_i64x2, |x| x.wrapping_neg()),
        0xc3 => {
            let v = frame.pop();
            frame.push_i32(i32::from(v_u64x2(v).iter().all(|&x| x != 0)));
        }
        0xc4 => {
            let v = frame.pop();
            frame.push_i32(simd_bitmask(&v_u64x2(v), 64));
        }
        0xc7 => simd_extend!(frame, v_i32x4, p_i64x2, i64, 2, false),
        0xc8 => simd_extend!(frame, v_i32x4, p_i64x2, i64, 2, true),
        0xc9 => simd_extend!(frame, v_u32x4, p_u64x2, u64, 2, false),
        0xca => simd_extend!(frame, v_u32x4, p_u64x2, u64, 2, true),
        0xcb => simd_shift!(frame, v_u64x2, p_u64x2, 64, <<),
        0xcc => simd_shift!(frame, v_i64x2, p_i64x2, 64, >>),
        0xcd => simd_shift!(frame, v_u64x2, p_u64x2, 64, >>),
        0xce => simd_bin!(frame, v_u64x2, p_u64x2, |a, b| a.wrapping_add(b)),
        0xd1 => simd_bin!(frame, v_u64x2, p_u64x2, |a, b| a.wrapping_sub(b)),
        0xd5 => simd_bin!(frame, v_u64x2, p_u64x2, |a, b| a.wrapping_mul(b)),
        0xdc => simd_extmul!(frame, v_i32x4, p_i64x2, i64, 2, false),
        0xdd => simd_extmul!(frame, v_i32x4, p_i64x2, i64, 2, true),
        0xde => simd_extmul!(frame, v_u32x4, p_u64x2, u64, 2, false),
        0xdf => simd_extmul!(frame, v_u32x4, p_u64x2, u64, 2, true),
        // --- f32x4 ---
        0x67 => simd_un!(frame, v_f32x4, p_f32x4, |x| ceil_f32(x)),
        0x68 => simd_un!(frame, v_f32x4, p_f32x4, |x| floor_f32(x)),
        0x69 => simd_un!(frame, v_f32x4, p_f32x4, |x| trunc_f32(x)),
        0x6a => simd_un!(frame, v_f32x4, p_f32x4, |x| nearest_f32(x)),
        0xe0 => simd_un!(frame, v_f32x4, p_f32x4, |x| fabs_f32(x)),
        0xe1 => simd_un!(frame, v_f32x4, p_f32x4, |x| fneg_f32(x)),
        0xe3 => {
            let a = v_f32x4(frame.pop());
            let mut r = a;
            for (i, slot) in r.iter_mut().enumerate() {
                *slot = sqrt_f32(a[i])?;
            }
            frame.push(p_f32x4(r));
        }
        0xe4 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a + b),
        0xe5 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a - b),
        0xe6 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a * b),
        0xe7 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| a / b),
        0xe8 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmin_f32(a, b)),
        0xe9 => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmax_f32(a, b)),
        0xea => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| if b < a { b } else { a }),
        0xeb => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| if a < b { b } else { a }),
        // --- f64x2 ---
        0x74 => simd_un!(frame, v_f64x2, p_f64x2, |x| ceil_f64(x)),
        0x75 => simd_un!(frame, v_f64x2, p_f64x2, |x| floor_f64(x)),
        0x7a => simd_un!(frame, v_f64x2, p_f64x2, |x| trunc_f64(x)),
        0x94 => simd_un!(frame, v_f64x2, p_f64x2, |x| nearest_f64(x)),
        0xec => simd_un!(frame, v_f64x2, p_f64x2, |x| fabs_f64(x)),
        0xed => simd_un!(frame, v_f64x2, p_f64x2, |x| fneg_f64(x)),
        0xef => {
            let a = v_f64x2(frame.pop());
            let mut r = a;
            for (i, slot) in r.iter_mut().enumerate() {
                *slot = sqrt_f64(a[i])?;
            }
            frame.push(p_f64x2(r));
        }
        0xf0 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a + b),
        0xf1 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a - b),
        0xf2 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a * b),
        0xf3 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| a / b),
        0xf4 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmin_f64(a, b)),
        0xf5 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmax_f64(a, b)),
        0xf6 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| if b < a { b } else { a }),
        0xf7 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| if a < b { b } else { a }),
        // --- saturating add/sub group members already handled above; narrow group ---
        0x65 => simd_narrow!(frame, v_i16x8, p_i8x16, i16, i8),
        0x66 => simd_narrow!(frame, v_i16x8, p_u8x16, i16, u8),
        // --- int<->float conversions ---
        0xfa => simd_convert!(frame, v_i32x4, p_f32x4, f32),
        0xfb => simd_convert!(frame, v_u32x4, p_f32x4, f32),
        0xfe => simd_convert!(frame, v_i32x4, p_f64x2, f64),
        0xff => simd_convert!(frame, v_u32x4, p_f64x2, f64),
        0xf8 | 0x101 => {
            let s4 = v_f32x4(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| sat_trunc_i32(f64::from(s4[i])))));
        }
        0xf9 | 0x102 => {
            let s4 = v_f32x4(frame.pop());
            frame.push(p_u32x4(core::array::from_fn(|i| sat_trunc_u32(f64::from(s4[i])))));
        }
        0xfc | 0x103 => {
            let s2 = v_f64x2(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| if i < 2 { sat_trunc_i32(s2[i]) } else { 0 })));
        }
        0xfd | 0x104 => {
            let s2 = v_f64x2(frame.pop());
            frame.push(p_u32x4(core::array::from_fn(|i| if i < 2 { sat_trunc_u32(s2[i]) } else { 0 })));
        }
        0x5e => {
            let s2 = v_f64x2(frame.pop());
            frame.push(p_f32x4([s2[0] as f32, s2[1] as f32, 0.0, 0.0]));
        }
        0x5f => {
            let s4 = v_f32x4(frame.pop());
            frame.push(p_f64x2([f64::from(s4[0]), f64::from(s4[1])]));
        }
        // --- widening loads / splat / zero ---
        0x01 => simd_load_extend!(frame, store, maps, s.mem, i8, 1, 8, p_i16x8, i16),
        0x02 => simd_load_extend!(frame, store, maps, s.mem, u8, 1, 8, p_u16x8, u16),
        0x03 => simd_load_extend!(frame, store, maps, s.mem, i16, 2, 4, p_i32x4, i32),
        0x04 => simd_load_extend!(frame, store, maps, s.mem, u16, 2, 4, p_u32x4, u32),
        0x05 => simd_load_extend!(frame, store, maps, s.mem, i32, 4, 2, p_i64x2, i64),
        0x06 => simd_load_extend!(frame, store, maps, s.mem, u32, 4, 2, p_u64x2, u64),
        0x07 => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 1)?;
            let x = store.memories[maps.mem(s.mem.memory)].bytes[ea];
            frame.push(p_u8x16([x; 16]));
        }
        0x08 => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 2)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            let x = u16::from_le_bytes(m.bytes[ea..ea + 2].try_into().unwrap());
            frame.push(p_u16x8([x; 8]));
        }
        0x09 => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 4)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            let x = u32::from_le_bytes(m.bytes[ea..ea + 4].try_into().unwrap());
            frame.push(p_u32x4([x; 4]));
        }
        0x0a => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 8)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            let x = u64::from_le_bytes(m.bytes[ea..ea + 8].try_into().unwrap());
            frame.push(p_u64x2([x; 2]));
        }
        0x5c => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 4)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&m.bytes[ea..ea + 4]);
            frame.push(Value::from_le_bytes(b));
        }
        0x5d => {
            let ea = simd_mem_ea(frame, store, maps, s.mem, 8)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            let mut b = [0u8; 16];
            b[0..8].copy_from_slice(&m.bytes[ea..ea + 8]);
            frame.push(Value::from_le_bytes(b));
        }
        // --- load_lane / store_lane ---
        0x54 => {
            let mut a = v_u8x16(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 1)?;
            a[lane] = store.memories[maps.mem(s.mem.memory)].bytes[ea];
            frame.push(p_u8x16(a));
        }
        0x55 => {
            let mut a = v_u16x8(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 2)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            a[lane] = u16::from_le_bytes(m.bytes[ea..ea + 2].try_into().unwrap());
            frame.push(p_u16x8(a));
        }
        0x56 => {
            let mut a = v_u32x4(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 4)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            a[lane] = u32::from_le_bytes(m.bytes[ea..ea + 4].try_into().unwrap());
            frame.push(p_u32x4(a));
        }
        0x57 => {
            let mut a = v_u64x2(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 8)?;
            let m = &store.memories[maps.mem(s.mem.memory)];
            a[lane] = u64::from_le_bytes(m.bytes[ea..ea + 8].try_into().unwrap());
            frame.push(p_u64x2(a));
        }
        0x58 => {
            let a = v_u8x16(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 1)?;
            store.memories[maps.mem(s.mem.memory)].bytes[ea] = a[lane];
        }
        0x59 => {
            let a = v_u16x8(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 2)?;
            store.memories[maps.mem(s.mem.memory)].bytes[ea..ea + 2].copy_from_slice(&a[lane].to_le_bytes());
        }
        0x5a => {
            let a = v_u32x4(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 4)?;
            store.memories[maps.mem(s.mem.memory)].bytes[ea..ea + 4].copy_from_slice(&a[lane].to_le_bytes());
        }
        0x5b => {
            let a = v_u64x2(frame.pop());
            let ea = simd_mem_ea(frame, store, maps, s.mem, 8)?;
            store.memories[maps.mem(s.mem.memory)].bytes[ea..ea + 8].copy_from_slice(&a[lane].to_le_bytes());
        }
        // --- relaxed SIMD (deterministic choices per wazmrt) ---
        0x105 => {
            let c = v_f32x4(frame.pop());
            let b = v_f32x4(frame.pop());
            let a = v_f32x4(frame.pop());
            frame.push(p_f32x4(core::array::from_fn(|i| a[i] * b[i] + c[i])));
        }
        0x106 => {
            let c = v_f32x4(frame.pop());
            let b = v_f32x4(frame.pop());
            let a = v_f32x4(frame.pop());
            frame.push(p_f32x4(core::array::from_fn(|i| c[i] - a[i] * b[i])));
        }
        0x107 => {
            let c = v_f64x2(frame.pop());
            let b = v_f64x2(frame.pop());
            let a = v_f64x2(frame.pop());
            frame.push(p_f64x2(core::array::from_fn(|i| a[i] * b[i] + c[i])));
        }
        0x108 => {
            let c = v_f64x2(frame.pop());
            let b = v_f64x2(frame.pop());
            let a = v_f64x2(frame.pop());
            frame.push(p_f64x2(core::array::from_fn(|i| c[i] - a[i] * b[i])));
        }
        0x109..=0x10c => {
            let m = frame.pop();
            let b = frame.pop();
            let a = frame.pop();
            frame.push((a & m) | (b & !m));
        }
        0x10d => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmin_f32(a, b)),
        0x10e => simd_bin!(frame, v_f32x4, p_f32x4, |a, b| fmax_f32(a, b)),
        0x10f => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmin_f64(a, b)),
        0x110 => simd_bin!(frame, v_f64x2, p_f64x2, |a, b| fmax_f64(a, b)),
        0x111 => {
            let b = v_i16x8(frame.pop());
            let a = v_i16x8(frame.pop());
            frame.push(p_i16x8(core::array::from_fn(|i| {
                let p = (i32::from(a[i]) * i32::from(b[i]) + 0x4000) >> 15;
                p.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })));
        }
        0x112 => {
            let b = v_i8x16(frame.pop());
            let a = v_i8x16(frame.pop());
            frame.push(p_i16x8(core::array::from_fn(|i| {
                let s = i32::from(a[2 * i]) * i32::from(b[2 * i])
                    + i32::from(a[2 * i + 1]) * i32::from(b[2 * i + 1]);
                s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })));
        }
        0x113 => {
            let c = v_i32x4(frame.pop());
            let b = v_i8x16(frame.pop());
            let a = v_i8x16(frame.pop());
            frame.push(p_i32x4(core::array::from_fn(|i| {
                let p0 = i32::from(a[4 * i]) * i32::from(b[4 * i])
                    + i32::from(a[4 * i + 1]) * i32::from(b[4 * i + 1]);
                let p1 = i32::from(a[4 * i + 2]) * i32::from(b[4 * i + 2])
                    + i32::from(a[4 * i + 3]) * i32::from(b[4 * i + 3]);
                p0.wrapping_add(p1).wrapping_add(c[i])
            })));
        }
        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

/// Pack each lane's high (sign) bit into the low bits of an i32 (`iNxM.bitmask`).
fn simd_bitmask<T: Copy + Into<u128>>(lanes: &[T], bits: u32) -> i32 {
    let mut m: u32 = 0;
    for (i, &x) in lanes.iter().enumerate() {
        if (x.into() >> (bits - 1)) & 1 != 0 {
            m |= 1 << i;
        }
    }
    m as i32
}

// ========================= Threads / atomics (0xFE) ========================
//
// Single-threaded: every atomic access is trivially atomic and `atomic.fence` is
// a no-op. Atomic accesses trap on a misaligned effective address (unlike ordinary
// loads/stores), and `wait*` requires a shared memory. `wait*` never blocks — a
// value mismatch returns 1 ("not equal"), a match returns 2 ("timed out", since no
// thread can notify); `notify` wakes 0. Ported from wazmrt `interp.zig execAtomic`.

/// Read `width` (1/2/4/8) little-endian bytes at `mem[ea..]`, zero-extended to u64.
fn atomic_read(mem: &[u8], ea: usize, width: u64) -> u64 {
    match width {
        1 => u64::from(mem[ea]),
        2 => u64::from(u16::from_le_bytes(mem[ea..ea + 2].try_into().unwrap())),
        4 => u64::from(u32::from_le_bytes(mem[ea..ea + 4].try_into().unwrap())),
        _ => u64::from_le_bytes(mem[ea..ea + 8].try_into().unwrap()),
    }
}
/// Write the low `width` bytes of `v` little-endian at `mem[ea..]`.
fn atomic_write(mem: &mut [u8], ea: usize, width: u64, v: u64) {
    match width {
        1 => mem[ea] = v as u8,
        2 => mem[ea..ea + 2].copy_from_slice(&(v as u16).to_le_bytes()),
        4 => mem[ea..ea + 4].copy_from_slice(&(v as u32).to_le_bytes()),
        _ => mem[ea..ea + 8].copy_from_slice(&v.to_le_bytes()),
    }
}
/// Keep only the low `width` bytes of `v` (a sub-width atomic op's access width).
fn mask_width(v: u64, width: u64) -> u64 {
    if width >= 8 {
        v
    } else {
        v & ((1u64 << (width * 8)) - 1)
    }
}
/// True if the atomic op at sub-opcode `sub` operates on an `i64` (vs `i32`).
fn atomic_is64(sub: u32) -> bool {
    match sub {
        0x11 | 0x14 | 0x15 | 0x16 | 0x18 | 0x1b | 0x1c | 0x1d => true,
        0x10 | 0x12 | 0x13 | 0x17 | 0x19 | 0x1a => false,
        // rmw/cmpxchg 7-op group [i32.full, i64.full, i32.8, i32.16, i64.8, i64.16, i64.32]
        _ => matches!((sub - 0x1e) % 7, 1 | 4 | 5 | 6),
    }
}
/// log2 of an atomic op's natural access width in bytes (its required alignment).
fn atomic_align_log2(sub: u32) -> u32 {
    match sub {
        0x00 | 0x01 => 2, // notify, wait32
        0x02 => 3,        // wait64
        0x03 => 0,        // fence (no memarg)
        0x10 | 0x17 => 2, // i32 load/store
        0x11 | 0x18 => 3, // i64 load/store
        0x12 | 0x19 => 0, // i32 …8
        0x13 | 0x1a => 1, // i32 …16
        0x14 | 0x1b => 0, // i64 …8
        0x15 | 0x1c => 1, // i64 …16
        0x16 | 0x1d => 2, // i64 …32
        _ => match (sub.wrapping_sub(0x1e)) % 7 {
            0 => 2, // i32 full
            1 => 3, // i64 full
            2 => 0, // i32.8
            3 => 1, // i32.16
            4 => 0, // i64.8
            5 => 1, // i64.16
            _ => 2, // i64.32
        },
    }
}

/// Pop an atomic op's address, bounds- + **alignment**-check it, require a shared
/// memory when `need_shared`. Returns the effective byte offset into `at.mem.memory`.
fn atomic_ea(
    frame: &mut Frame,
    store: &Pools,
    maps: &IndexMaps,
    at: crate::opcode::Atomic,
    width: u64,
    need_shared: bool,
) -> Result<usize> {
    let mem = store.memories.get(maps.mem(at.mem.memory)).ok_or(Trap::NoMemory)?;
    let base = frame.pop_mem(mem.is64);
    if need_shared && !mem.shared {
        return Err(Trap::ExpectedSharedMemory);
    }
    let ea = base.checked_add(at.mem.offset).ok_or(Trap::MemoryOutOfBounds)?;
    let end = ea.checked_add(width).ok_or(Trap::MemoryOutOfBounds)?;
    if end > mem.bytes.len() as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    if ea % width != 0 {
        return Err(Trap::UnalignedAtomic);
    }
    Ok(ea as usize)
}

/// Execute a `0xFE` atomic instruction (single-threaded semantics).
fn exec_atomic(frame: &mut Frame, store: &mut Pools, maps: &IndexMaps, at: crate::opcode::Atomic) -> Result<()> {
    let sub = at.sub;
    if sub == 0x03 {
        return Ok(()); // atomic.fence — nothing to order single-threaded
    }
    let w = 1u64 << atomic_align_log2(sub);
    let mi = maps.mem(at.mem.memory);
    match sub {
        0x00 => {
            // memory.atomic.notify [addr count] -> [woken] (always 0 single-threaded)
            let _count = frame.pop_i32();
            let _ea = atomic_ea(frame, store, maps, at, w, false)?;
            frame.push_i32(0);
        }
        0x01 | 0x02 => {
            // memory.atomic.wait32 / wait64 [addr expected timeout] -> [i32]
            let _timeout = frame.pop_i64();
            let expected: u64 = if sub == 0x01 {
                u64::from(frame.pop_i32() as u32)
            } else {
                frame.pop_i64() as u64
            };
            let ea = atomic_ea(frame, store, maps, at, w, true)?;
            let cur = atomic_read(&store.memories[mi].bytes, ea, w);
            frame.push_i32(if cur != expected { 1 } else { 2 });
        }
        0x10..=0x16 => {
            // atomic load [addr] -> [T]
            let ea = atomic_ea(frame, store, maps, at, w, false)?;
            let v = atomic_read(&store.memories[mi].bytes, ea, w);
            if atomic_is64(sub) {
                frame.push_i64(v as i64);
            } else {
                frame.push_i32(v as u32 as i32);
            }
        }
        0x17..=0x1d => {
            // atomic store [addr T] -> []
            let val: u64 = if atomic_is64(sub) {
                frame.pop_i64() as u64
            } else {
                u64::from(frame.pop_i32() as u32)
            };
            let ea = atomic_ea(frame, store, maps, at, w, false)?;
            atomic_write(&mut store.memories[mi].bytes, ea, w, val);
        }
        0x1e..=0x4e => {
            let is64 = atomic_is64(sub);
            let group = (sub - 0x1e) / 7; // 0 add,1 sub,2 and,3 or,4 xor,5 xchg,6 cmpxchg
            if group == 6 {
                // cmpxchg [addr expected replacement] -> [old]
                let repl: u64 = if is64 {
                    frame.pop_i64() as u64
                } else {
                    u64::from(frame.pop_i32() as u32)
                };
                let expected: u64 = if is64 {
                    frame.pop_i64() as u64
                } else {
                    u64::from(frame.pop_i32() as u32)
                };
                let ea = atomic_ea(frame, store, maps, at, w, false)?;
                let old = atomic_read(&store.memories[mi].bytes, ea, w);
                if old == mask_width(expected, w) {
                    atomic_write(&mut store.memories[mi].bytes, ea, w, repl);
                }
                if is64 {
                    frame.push_i64(old as i64);
                } else {
                    frame.push_i32(old as u32 as i32);
                }
            } else {
                // add/sub/and/or/xor/xchg [addr val] -> [old]
                let val: u64 = if is64 {
                    frame.pop_i64() as u64
                } else {
                    u64::from(frame.pop_i32() as u32)
                };
                let ea = atomic_ea(frame, store, maps, at, w, false)?;
                let old = atomic_read(&store.memories[mi].bytes, ea, w);
                let new = match group {
                    0 => old.wrapping_add(val),
                    1 => old.wrapping_sub(val),
                    2 => old & val,
                    3 => old | val,
                    4 => old ^ val,
                    _ => val, // xchg
                };
                atomic_write(&mut store.memories[mi].bytes, ea, w, new);
                if is64 {
                    frame.push_i64(old as i64);
                } else {
                    frame.push_i32(old as u32 as i32);
                }
            }
        }
        _ => return Err(Trap::UnsupportedInstruction),
    }
    Ok(())
}

fn cmp_i32(op: u8, a: i32, b: i32) -> bool {
    let (ua, ub) = (a as u32, b as u32);
    match op {
        0x46 => a == b,
        0x47 => a != b,
        0x48 => a < b,
        0x49 => ua < ub,
        0x4a => a > b,
        0x4b => ua > ub,
        0x4c => a <= b,
        0x4d => ua <= ub,
        0x4e => a >= b,
        _ => ua >= ub, // 0x4f i32.ge_u
    }
}

fn cmp_i64(op: u8, a: i64, b: i64) -> bool {
    let (ua, ub) = (a as u64, b as u64);
    match op {
        0x51 => a == b,
        0x52 => a != b,
        0x53 => a < b,
        0x54 => ua < ub,
        0x55 => a > b,
        0x56 => ua > ub,
        0x57 => a <= b,
        0x58 => ua <= ub,
        0x59 => a >= b,
        _ => ua >= ub, // 0x5a i64.ge_u
    }
}

fn bin_i32(op: u8, a: i32, b: i32) -> Result<i32> {
    let (ua, ub) = (a as u32, b as u32);
    Ok(match op {
        0x6a => a.wrapping_add(b),
        0x6b => a.wrapping_sub(b),
        0x6c => a.wrapping_mul(b),
        0x6d => {
            // div_s
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if a == i32::MIN && b == -1 {
                return Err(Trap::IntOverflow);
            }
            a / b
        }
        0x6e => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua / ub) as i32
        }
        0x6f => {
            // rem_s
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if b == -1 {
                0
            } else {
                a % b
            }
        }
        0x70 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua % ub) as i32
        }
        0x71 => a & b,
        0x72 => a | b,
        0x73 => a ^ b,
        0x74 => (ua << (ub % 32)) as i32,
        0x75 => a >> (ub % 32),
        0x76 => (ua >> (ub % 32)) as i32,
        0x77 => ua.rotate_left(ub % 32) as i32,
        _ => ua.rotate_right(ub % 32) as i32, // 0x78 i32.rotr
    })
}

fn bin_i64(op: u8, a: i64, b: i64) -> Result<i64> {
    let (ua, ub) = (a as u64, b as u64);
    Ok(match op {
        0x7c => a.wrapping_add(b),
        0x7d => a.wrapping_sub(b),
        0x7e => a.wrapping_mul(b),
        0x7f => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if a == i64::MIN && b == -1 {
                return Err(Trap::IntOverflow);
            }
            a / b
        }
        0x80 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua / ub) as i64
        }
        0x81 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            if b == -1 {
                0
            } else {
                a % b
            }
        }
        0x82 => {
            if b == 0 {
                return Err(Trap::DivByZero);
            }
            (ua % ub) as i64
        }
        0x83 => a & b,
        0x84 => a | b,
        0x85 => a ^ b,
        0x86 => (ua << (ub % 64)) as i64,
        0x87 => a >> (ub % 64),
        0x88 => (ua >> (ub % 64)) as i64,
        0x89 => ua.rotate_left((ub % 64) as u32) as i64,
        _ => ua.rotate_right((ub % 64) as u32) as i64, // 0x8a i64.rotr
    })
}

/// Evaluate a defined global's constant initializer (integer/float const, `global.get` of a
/// prior global, and extended-const i32/i64 add/sub/mul). Reference and GC const-exprs are
/// deferred with the reference-type execution slice.
/// Evaluate a constant expression. `inst` is the index of the instance this expression belongs to,
/// needed because `ref.func` produces a funcref and a funcref carries its owning instance.
fn eval_const_expr(
    expr: &[u8],
    globals: &[Value],
    inst: usize,
    gc: Option<(&Module, &mut Pools)>,
) -> Result<Value> {
    let mut r = Reader::new(expr);
    let mut stack: Vec<Value> = Vec::new();
    // The GC constant forms need the module (for field layouts) and the heap (to allocate into).
    // `None` at the sites that cannot produce one — a segment *offset* is an integer — so those keep
    // rejecting `struct.new` and friends rather than being handed a heap they have no use for.
    let mut gc = gc;
    loop {
        match r.read_byte()? {
            0x0b => break,
            0x41 => stack.push(i32_value(r.read_var_i32()?)),
            0x42 => stack.push(i64_value(r.read_var_i64()?)),
            0x43 => stack.push(Value::from(r.read_f32_bits()?)),
            0x44 => stack.push(Value::from(r.read_f64_bits()?)),
            0x23 => {
                let gi = r.read_var_u32()? as usize;
                stack.push(*globals.get(gi).ok_or(Trap::UndefinedGlobal)?);
            }
            0xd0 => {
                // ref.null <heaptype> — the heap type is consumed; the value is the sentinel.
                crate::opcode::read_heap_type(&mut r).map_err(|_| Trap::ConstantExpr)?;
                stack.push(NULL_REF);
            }
            0xd2 => {
                // ref.func x — stamped with the instance this expression belongs to, exactly as the
                // instruction form is. A table initializer that will be shared with another instance
                // must still produce references callable against THIS one.
                stack.push(pack_funcref(inst, r.read_var_u32()?));
            }
            // The GC constant forms (§3.3.11). Rejected by both validator and interpreter until now,
            // which was consistent but cost far more than its logged size: a global initializer that
            // fails to validate stops the whole *module* building, and every later assertion in the
            // file is then skipped for want of a target.
            0xfb => {
                let (module, pools) = gc.as_mut().ok_or(Trap::ConstantExpr)?;
                match r.read_var_u32()? {
                    // struct.new t — fields popped in declaration order.
                    0x00 => {
                        let ti = r.read_var_u32()?;
                        let sf = module.struct_fields(ti).ok_or(Trap::UndefinedType)?;
                        let base = stack.len().checked_sub(sf.len()).ok_or(Trap::ConstantExpr)?;
                        let obj: Vec<Value> = sf
                            .iter()
                            .enumerate()
                            .map(|(k, f)| pack_field(f.storage, stack[base + k]))
                            .collect();
                        stack.truncate(base);
                        let v = alloc_object(pools, inst, ti, obj)?;
                        stack.push(v);
                    }
                    // struct.new_default t
                    0x01 => {
                        let ti = r.read_var_u32()?;
                        let sf = module.struct_fields(ti).ok_or(Trap::UndefinedType)?;
                        let obj: Vec<Value> = sf.iter().map(|f| default_field(f.storage)).collect();
                        let v = alloc_object(pools, inst, ti, obj)?;
                        stack.push(v);
                    }
                    // array.new t — (init, len)
                    0x06 => {
                        let ti = r.read_var_u32()?;
                        let f = module.array_field(ti).ok_or(Trap::UndefinedType)?;
                        let n = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?) as u32 as usize;
                        let init = pack_field(f.storage, stack.pop().ok_or(Trap::ConstantExpr)?);
                        let v = alloc_object(pools, inst, ti, vec![init; n])?;
                        stack.push(v);
                    }
                    // array.new_default t — (len)
                    0x07 => {
                        let ti = r.read_var_u32()?;
                        let f = module.array_field(ti).ok_or(Trap::UndefinedType)?;
                        let n = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?) as u32 as usize;
                        let v = alloc_object(pools, inst, ti, vec![default_field(f.storage); n])?;
                        stack.push(v);
                    }
                    // array.new_fixed t n — n elements already on the stack.
                    0x08 => {
                        let ti = r.read_var_u32()?;
                        let n = r.read_var_u32()? as usize;
                        let f = module.array_field(ti).ok_or(Trap::UndefinedType)?;
                        let base = stack.len().checked_sub(n).ok_or(Trap::ConstantExpr)?;
                        let elems: Vec<Value> = stack[base..]
                            .iter()
                            .map(|&v| pack_field(f.storage, v))
                            .collect();
                        stack.truncate(base);
                        let v = alloc_object(pools, inst, ti, elems)?;
                        stack.push(v);
                    }
                    // The externref bridge — constant per §3.3.11, and `extern.wast` opens with
                    // `(global externref (extern.convert_any (ref.null any)))`, so a module using
                    // it does not BUILD without these two arms.
                    0x1a => {
                        let r = stack.pop().ok_or(Trap::ConstantExpr)?;
                        stack.push(internalize(r));
                    }
                    0x1b => {
                        let r = stack.pop().ok_or(Trap::ConstantExpr)?;
                        stack.push(externalize(r));
                    }
                    // ref.i31 — unboxed, so no allocation. `NULL_REF` is checked before `I31_TAG`
                    // everywhere that reads a reference, which is what keeps the two apart.
                    0x1c => {
                        let x = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?);
                        stack.push(I31_TAG | Value::from(x as u32 & 0x7fff_ffff));
                    }
                    _ => return Err(Trap::ConstantExpr),
                }
            }
            byte @ 0x6a..=0x6c => {
                let b = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?);
                let a = as_i32(stack.pop().ok_or(Trap::ConstantExpr)?);
                stack.push(i32_value(bin_i32(byte, a, b)?));
            }
            byte @ 0x7c..=0x7e => {
                let b = as_i64(stack.pop().ok_or(Trap::ConstantExpr)?);
                let a = as_i64(stack.pop().ok_or(Trap::ConstantExpr)?);
                stack.push(i64_value(bin_i64(byte, a, b)?));
            }
            0xfd => {
                // v128.const — the only 0xFD op valid in a constant expression.
                if r.read_var_u32()? != 0x0c {
                    return Err(Trap::ConstantExpr);
                }
                let mut b = [0u8; 16];
                for slot in &mut b {
                    *slot = r.read_byte()?;
                }
                stack.push(Value::from_le_bytes(b));
            }
            _ => return Err(Trap::ConstantExpr),
        }
    }
    stack.pop().ok_or(Trap::ConstantExpr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::decode;

    /// Build a single-function module: one type, one function, one export, one code entry.
    /// `code_entry` is the locals-vector + body bytes (including the trailing `end`).
    fn single_func(params: &[u8], results: &[u8], code_entry: &[u8], export: &str) -> Vec<u8> {
        fn section(id: u8, content: &[u8]) -> Vec<u8> {
            let mut s = vec![id];
            write_uleb(&mut s, content.len() as u32);
            s.extend_from_slice(content);
            s
        }
        let mut type_content = vec![0x01u8, 0x60];
        write_uleb(&mut type_content, params.len() as u32);
        type_content.extend_from_slice(params);
        write_uleb(&mut type_content, results.len() as u32);
        type_content.extend_from_slice(results);

        let mut export_content = vec![0x01u8];
        write_uleb(&mut export_content, export.len() as u32);
        export_content.extend_from_slice(export.as_bytes());
        export_content.extend_from_slice(&[0x00, 0x00]); // kind func, index 0

        let mut code_content = vec![0x01u8];
        write_uleb(&mut code_content, code_entry.len() as u32);
        code_content.extend_from_slice(code_entry);

        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        m.extend(section(0x01, &type_content));
        m.extend(section(0x03, &[0x01, 0x00])); // function: 1 func of type 0
        m.extend(section(0x07, &export_content));
        m.extend(section(0x0a, &code_content));
        m
    }

    fn run1(bytes: &[u8], export: &str, args: &[Value]) -> Result<Vec<Value>> {
        let md = decode(bytes).unwrap();
        let mut inst = Instance::new(md)?;
        inst.invoke(export, args)
    }

    #[test]
    fn runs_add() {
        // (func (param i32 i32) (result i32) local.get 0  local.get 1  i32.add)
        let entry = [0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b];
        let m = single_func(&[0x7f, 0x7f], &[0x7f], &entry, "add");
        let r = run1(&m, "add", &[i32_value(40), i32_value(2)]).unwrap();
        assert_eq!(as_i32(r[0]), 42);
    }

    #[test]
    fn runs_recursive_factorial() {
        // (func $fac (param i32) (result i32)
        //   (if (result i32) (i32.lt_s (local.get 0) (i32.const 1))
        //     (then (i32.const 1))
        //     (else (i32.mul (local.get 0) (call 0 (i32.sub (local.get 0) (i32.const 1)))))))
        let entry = [
            0x00, // no locals
            0x20, 0x00, // local.get 0
            0x41, 0x01, // i32.const 1
            0x48, // i32.lt_s
            0x04, 0x7f, // if (result i32)
            0x41, 0x01, // i32.const 1
            0x05, // else
            0x20, 0x00, // local.get 0
            0x20, 0x00, // local.get 0
            0x41, 0x01, // i32.const 1
            0x6b, // i32.sub
            0x10, 0x00, // call 0
            0x6c, // i32.mul
            0x0b, // end (if)
            0x0b, // end (func)
        ];
        let m = single_func(&[0x7f], &[0x7f], &entry, "fac");
        assert_eq!(as_i32(run1(&m, "fac", &[i32_value(5)]).unwrap()[0]), 120);
        assert_eq!(as_i32(run1(&m, "fac", &[i32_value(0)]).unwrap()[0]), 1);
        assert_eq!(as_i32(run1(&m, "fac", &[i32_value(10)]).unwrap()[0]), 3_628_800);
    }

    #[test]
    fn runs_loop_sum() {
        // sum(n) = n + (n-1) + ... + 1, via a block/loop with br_if/br.
        let entry = [
            0x01, 0x01, 0x7f, // one i32 local (the accumulator, local 1)
            0x02, 0x40, // block
            0x03, 0x40, // loop
            0x20, 0x00, // local.get 0
            0x45, // i32.eqz
            0x0d, 0x01, // br_if 1  (exit block when n == 0)
            0x20, 0x01, // local.get 1  (acc)
            0x20, 0x00, // local.get 0  (n)
            0x6a, // i32.add
            0x21, 0x01, // local.set 1  (acc += n)
            0x20, 0x00, // local.get 0
            0x41, 0x01, // i32.const 1
            0x6b, // i32.sub
            0x21, 0x00, // local.set 0  (n -= 1)
            0x0c, 0x00, // br 0  (continue loop)
            0x0b, // end (loop)
            0x0b, // end (block)
            0x20, 0x01, // local.get 1  (return acc)
            0x0b, // end (func)
        ];
        let m = single_func(&[0x7f], &[0x7f], &entry, "sum");
        assert_eq!(as_i32(run1(&m, "sum", &[i32_value(5)]).unwrap()[0]), 15);
        assert_eq!(as_i32(run1(&m, "sum", &[i32_value(100)]).unwrap()[0]), 5050);
    }

    #[test]
    fn traps_divide_by_zero() {
        // (func (param i32) (result i32) local.get 0  i32.const 0  i32.div_s)
        let entry = [0x00, 0x20, 0x00, 0x41, 0x00, 0x6d, 0x0b];
        let m = single_func(&[0x7f], &[0x7f], &entry, "d");
        assert_eq!(run1(&m, "d", &[i32_value(1)]), Err(Trap::DivByZero));
    }

    #[test]
    fn traps_unreachable() {
        let entry = [0x00, 0x00, 0x0b]; // unreachable ; end
        let m = single_func(&[], &[], &entry, "u");
        assert_eq!(run1(&m, "u", &[]), Err(Trap::Unreachable));
    }

    #[test]
    fn i64_arithmetic() {
        // (func (param i64 i64) (result i64) local.get 0  local.get 1  i64.mul)
        let entry = [0x00, 0x20, 0x00, 0x20, 0x01, 0x7e, 0x0b];
        let m = single_func(&[0x7e, 0x7e], &[0x7e], &entry, "mul");
        let r = run1(&m, "mul", &[i64_value(1_000_000), i64_value(1_000_000)]).unwrap();
        assert_eq!(as_i64(r[0]), 1_000_000_000_000);
    }

    #[test]
    fn runs_f64_add() {
        // (func (param f64 f64) (result f64) local.get 0  local.get 1  f64.add)
        let entry = [0x00, 0x20, 0x00, 0x20, 0x01, 0xa0, 0x0b];
        let m = single_func(&[0x7c, 0x7c], &[0x7c], &entry, "add");
        let r = run1(&m, "add", &[f64_value(1.5), f64_value(2.25)]).unwrap();
        assert_eq!(as_f64(r[0]), 3.75);
    }

    #[test]
    fn nearest_rounds_ties_to_even() {
        assert_eq!(nearest_f64(2.5), 2.0);
        assert_eq!(nearest_f64(3.5), 4.0);
        assert_eq!(nearest_f64(-2.5), -2.0);
        assert_eq!(nearest_f64(0.4), 0.0);
        // -0.5 rounds to -0.0 (sign of zero preserved).
        assert_eq!(nearest_f64(-0.5).to_bits(), (-0.0f64).to_bits());
        assert_eq!(nearest_f32(2.5), 2.0);
        assert_eq!(nearest_f32(3.5), 4.0);
    }

    #[test]
    fn min_max_nan_and_signed_zero() {
        assert!(fmin_f32(f32::NAN, 1.0).is_nan());
        assert!(fmax_f32(1.0, f32::NAN).is_nan());
        // min(+0,-0) = -0 ; max(+0,-0) = +0.
        assert_eq!(fmin_f32(0.0, -0.0).to_bits(), (-0.0f32).to_bits());
        assert_eq!(fmax_f32(0.0, -0.0).to_bits(), (0.0f32).to_bits());
        assert_eq!(fmin_f64(2.0, 3.0), 2.0);
        assert_eq!(fmax_f64(2.0, 3.0), 3.0);
    }

    #[test]
    fn trunc_floor_ceil() {
        assert_eq!(trunc_f64(2.7), 2.0);
        assert_eq!(trunc_f64(-2.7), -2.0);
        assert_eq!(floor_f64(-2.1), -3.0);
        assert_eq!(ceil_f64(2.1), 3.0);
        assert_eq!(trunc_f32(-0.9).to_bits(), (-0.0f32).to_bits());
    }

    #[test]
    fn float_to_int_traps_and_saturates() {
        // i32.trunc_f64_s traps on an out-of-range value; i32.trunc_sat clamps.
        let trap_entry = [0x00, 0x20, 0x00, 0xaa, 0x0b]; // local.get 0 ; i32.trunc_f64_s
        let mt = single_func(&[0x7c], &[0x7f], &trap_entry, "t");
        assert_eq!(run1(&mt, "t", &[f64_value(1e300)]), Err(Trap::InvalidConversionToInt));
        assert_eq!(as_i32(run1(&mt, "t", &[f64_value(42.9)]).unwrap()[0]), 42);

        // i32.trunc_sat_f64_s (0xfc 0x02) saturates instead of trapping.
        let sat_entry = [0x00, 0x20, 0x00, 0xfc, 0x02, 0x0b];
        let ms = single_func(&[0x7c], &[0x7f], &sat_entry, "s");
        assert_eq!(as_i32(run1(&ms, "s", &[f64_value(1e300)]).unwrap()[0]), i32::MAX);
        assert_eq!(as_i32(run1(&ms, "s", &[f64_value(f64::NAN)]).unwrap()[0]), 0);
    }

    /// Assemble a module from `(section-id, content)` pairs (magic + version prepended).
    fn asm(sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        for (id, content) in sections {
            m.push(*id);
            write_uleb(&mut m, content.len() as u32);
            m.extend_from_slice(content);
        }
        m
    }
    /// Wrap a code body (locals-vec + instrs incl. `end`) as a 1-entry code section content.
    fn code1(entry: &[u8]) -> Vec<u8> {
        code_n(&[entry])
    }
    /// Wrap N code bodies as a code section content.
    fn code_n(entries: &[&[u8]]) -> Vec<u8> {
        let mut c = vec![entries.len() as u8];
        for e in entries {
            write_uleb(&mut c, e.len() as u32);
            c.extend_from_slice(e);
        }
        c
    }

    #[test]
    fn call_indirect_dispatch() {
        // table [add, sub]; dispatch(op, a, b) = table[op](a, b).
        let add = [0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b];
        let sub = [0x00, 0x20, 0x00, 0x20, 0x01, 0x6b, 0x0b];
        let disp = [0x00, 0x20, 0x01, 0x20, 0x02, 0x20, 0x00, 0x11, 0x00, 0x00, 0x0b];
        let m = asm(&[
            // type 0: (i32,i32)->i32 ; type 1: (i32,i32,i32)->i32
            (1, vec![
                0x02, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x03, 0x7f, 0x7f, 0x7f, 0x01, 0x7f,
            ]),
            (3, vec![0x03, 0x00, 0x00, 0x01]), // funcs of type 0, 0, 1
            (4, vec![0x01, 0x70, 0x00, 0x02]), // table: funcref, min 2
            (7, {
                let mut e = vec![0x01, 0x08];
                e.extend_from_slice(b"dispatch");
                e.extend_from_slice(&[0x00, 0x02]); // func index 2
                e
            }),
            (9, vec![0x01, 0x00, 0x41, 0x00, 0x0b, 0x02, 0x00, 0x01]), // active elem @0: [func 0, func 1]
            (10, code_n(&[&add, &sub, &disp])),
        ]);
        let d = |op, a, b| as_i32(run1(&m, "dispatch", &[i32_value(op), i32_value(a), i32_value(b)]).unwrap()[0]);
        assert_eq!(d(0, 40, 2), 42); // add
        assert_eq!(d(1, 40, 2), 38); // sub
        assert_eq!(
            run1(&m, "dispatch", &[i32_value(9), i32_value(1), i32_value(1)]),
            Err(Trap::TableOutOfBounds)
        );
    }

    #[test]
    fn ref_null_is_null() {
        // (func (result i32) ref.null func; ref.is_null)  -> 1
        let entry = [0x00, 0xd0, 0x70, 0xd1, 0x0b];
        let m = single_func(&[], &[0x7f], &entry, "n");
        assert_eq!(as_i32(run1(&m, "n", &[]).unwrap()[0]), 1);
    }

    #[test]
    fn table_set_get_ref_func() {
        // funcs: $g (empty) ; test: table.set[1]=ref.func $g ; ref.is_null(table.get[1]) -> 0
        let g = [0x00, 0x0b];
        let test = [
            0x00, 0x41, 0x01, 0xd2, 0x00, 0x26, 0x00, // (slot 1) (ref.func 0) table.set table 0
            0x41, 0x01, 0x25, 0x00, // (slot 1) table.get table 0
            0xd1, 0x0b, // ref.is_null
        ];
        let m = asm(&[
            (1, vec![0x02, 0x60, 0x00, 0x00, 0x60, 0x00, 0x01, 0x7f]), // ()->() ; ()->(i32)
            (3, vec![0x02, 0x00, 0x01]),
            (4, vec![0x01, 0x70, 0x00, 0x03]), // table funcref min 3
            (7, vec![0x01, 0x01, b't', 0x00, 0x01]),
            (10, code_n(&[&g, &test])),
        ]);
        assert_eq!(as_i32(run1(&m, "t", &[]).unwrap()[0]), 0); // slot 1 is non-null after set
    }

    #[test]
    fn memory_store_load_roundtrip() {
        // (memory 1) (func (param i32) (result i32) i32.store[0] (get 0); i32.load[0])
        let entry = [
            0x00, 0x41, 0x00, 0x20, 0x00, 0x36, 0x02, 0x00, // i32.store (addr 0) (local 0)
            0x41, 0x00, 0x28, 0x02, 0x00, // i32.load (addr 0)
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]), // memory: min 1
            (7, vec![0x01, 0x02, b'r', b't', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "rt", &[i32_value(0x1234_5678)]).unwrap()[0]), 0x1234_5678);
    }

    #[test]
    fn memory_size_and_grow() {
        // (memory 1) (func (result i32) i32.const 2; memory.grow; drop; memory.size)
        let entry = [0x00, 0x41, 0x02, 0x40, 0x00, 0x1a, 0x3f, 0x00, 0x0b];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'g', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "g", &[]).unwrap()[0]), 3); // 1 + 2 pages
    }

    #[test]
    fn active_data_segment_initializes_memory() {
        // (memory 1) (data (i32.const 0) "\2a") (func (result i32) i32.load8_u 0)
        let entry = [0x00, 0x41, 0x00, 0x2d, 0x00, 0x00, 0x0b];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'd', 0x00, 0x00]),
            (10, code1(&entry)),
            (11, vec![0x01, 0x00, 0x41, 0x00, 0x0b, 0x01, 0x2a]), // active seg @0, bytes [0x2a]
        ]);
        assert_eq!(as_i32(run1(&m, "d", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn memory_out_of_bounds_traps() {
        // (memory 1) (func (result i32) i32.load (i32.const 100000))  — past 1 page (65536)
        let entry = [0x00, 0x41, 0xa0, 0x8d, 0x06, 0x28, 0x02, 0x00, 0x0b];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'o', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(run1(&m, "o", &[]), Err(Trap::MemoryOutOfBounds));
    }

    #[test]
    fn gc_struct_new_set_get() {
        // type 0 = ()->i32 ; type 1 = (struct (field (mut i32)))
        // new_default, set field0=42, get field0 -> 42
        let entry = [
            0x01, 0x01, 0x63, 0x01, // 1 local: (ref null 1)
            0xfb, 0x01, 0x01, // struct.new_default 1
            0x21, 0x00, // local.set 0
            0x20, 0x00, 0x41, 0x2a, 0xfb, 0x05, 0x01, 0x00, // struct.set 1 0 = 42
            0x20, 0x00, 0xfb, 0x02, 0x01, 0x00, // struct.get 1 0
            0x0b,
        ];
        let m = asm(&[
            (
                1,
                vec![
                    0x02, // 2 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: ()->i32
                    0x5f, 0x01, 0x7f, 0x01, // type 1: struct { (mut i32) }
                ],
            ),
            (3, vec![0x01, 0x00]),
            (7, vec![0x01, 0x02, b's', b'm', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "sm", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn gc_array_new_set_get_len() {
        // type 0 = ()->i32 ; type 1 = (array (mut i32))
        // new 7×3, a[1]=50, a[1]+len -> 53
        let entry = [
            0x01, 0x01, 0x63, 0x01, // 1 local: (ref null 1)
            0x41, 0x07, 0x41, 0x03, 0xfb, 0x06, 0x01, // array.new 1 (init 7, len 3)
            0x21, 0x00, // local.set 0
            0x20, 0x00, 0x41, 0x01, 0x41, 0x32, 0xfb, 0x0e, 0x01, // array.set 1 (idx 1) = 50
            0x20, 0x00, 0x41, 0x01, 0xfb, 0x0b, 0x01, // array.get 1 (idx 1)
            0x20, 0x00, 0xfb, 0x0f, // array.len
            0x6a, 0x0b, // i32.add
        ];
        let m = asm(&[
            (
                1,
                vec![
                    0x02, // 2 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: ()->i32
                    0x5e, 0x7f, 0x01, // type 1: array (mut i32)
                ],
            ),
            (3, vec![0x01, 0x00]),
            (7, vec![0x01, 0x02, b'a', b'x', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "ax", &[]).unwrap()[0]), 53);
    }

    #[test]
    fn gc_i31() {
        // i31.get_s(ref.i31(-5)) -> -5
        let s = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x7b, 0xfb, 0x1c, 0xfb, 0x1d, 0x0b],
            "i",
        );
        assert_eq!(as_i32(run1(&s, "i", &[]).unwrap()[0]), -5);
        // i31.get_u(ref.i31(5)) -> 5
        let u = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x05, 0xfb, 0x1c, 0xfb, 0x1e, 0x0b],
            "u",
        );
        assert_eq!(as_i32(run1(&u, "u", &[]).unwrap()[0]), 5);
    }

    #[test]
    fn gc_ref_test_i31() {
        // ref.test (ref i31) (ref.i31 1) -> 1
        let t = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x01, 0xfb, 0x1c, 0xfb, 0x14, 0x6c, 0x0b],
            "t",
        );
        assert_eq!(as_i32(run1(&t, "t", &[]).unwrap()[0]), 1);
    }

    // --- SIMD (v128) ---

    #[test]
    fn simd_splat_extract() {
        // i32x4.extract_lane 2 (i32x4.splat 7) -> 7
        let s = single_func(
            &[],
            &[0x7f],
            &[0x00, 0x41, 0x07, 0xfd, 0x11, 0xfd, 0x1b, 0x02, 0x0b],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 7);
    }

    #[test]
    fn simd_const_extract_u() {
        // v128.const [0,1,..,15]; i8x16.extract_lane_u 3 -> 3
        let mut e = vec![0x00, 0xfd, 0x0c];
        e.extend(0u8..16);
        e.extend_from_slice(&[0xfd, 0x16, 0x03, 0x0b]);
        let s = single_func(&[], &[0x7f], &e, "s");
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 3);
    }

    #[test]
    fn simd_i32x4_add() {
        // extract_lane 0 (i32x4.add (splat 3) (splat 4)) -> 7
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0x03, 0xfd, 0x11, 0x41, 0x04, 0xfd, 0x11, 0xfd, 0xae, 0x01, 0xfd, 0x1b,
                0x00, 0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 7);
    }

    #[test]
    fn simd_eq_bitmask() {
        // i32x4.bitmask (i32x4.eq (splat 5) (splat 5)) -> 0b1111 = 15
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0x05, 0xfd, 0x11, 0x41, 0x05, 0xfd, 0x11, 0xfd, 0x37, 0xfd, 0xa4, 0x01,
                0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 15);
    }

    #[test]
    fn simd_shift() {
        // i32x4.extract_lane 0 (i32x4.shl (splat 1) 4) -> 16
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0x01, 0xfd, 0x11, 0x41, 0x04, 0xfd, 0xab, 0x01, 0xfd, 0x1b, 0x00, 0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 16);
    }

    #[test]
    fn simd_f32x4_add() {
        // f32x4.extract_lane 0 (f32x4.add (splat 2.5) (splat 1.5)) -> 4.0
        let s = single_func(
            &[],
            &[0x7d],
            &[
                0x00, //
                0x43, 0x00, 0x00, 0x20, 0x40, 0xfd, 0x13, // f32.const 2.5; f32x4.splat
                0x43, 0x00, 0x00, 0xc0, 0x3f, 0xfd, 0x13, // f32.const 1.5; f32x4.splat
                0xfd, 0xe4, 0x01, // f32x4.add
                0xfd, 0x1f, 0x00, // f32x4.extract_lane 0
                0x0b,
            ],
            "s",
        );
        assert_eq!(as_f32(run1(&s, "s", &[]).unwrap()[0]), 4.0);
    }

    #[test]
    fn simd_add_sat_s() {
        // i8x16.extract_lane_s 0 (i8x16.add_sat_s (splat 100) (splat 100)) -> 127 (saturated)
        let s = single_func(
            &[],
            &[0x7f],
            &[
                0x00, 0x41, 0xe4, 0x00, 0xfd, 0x0f, // i32.const 100; i8x16.splat
                0x41, 0xe4, 0x00, 0xfd, 0x0f, // i32.const 100; i8x16.splat
                0xfd, 0x6f, // i8x16.add_sat_s
                0xfd, 0x15, 0x00, // i8x16.extract_lane_s 0
                0x0b,
            ],
            "s",
        );
        assert_eq!(as_i32(run1(&s, "s", &[]).unwrap()[0]), 127);
    }

    #[test]
    fn simd_load_store_roundtrip() {
        // store a v128 [10,20,30,40] (i32 lanes) at addr 0, load it back, extract lane 2 -> 30
        let entry = [
            0x00, 0x41, 0x00, // i32.const 0 (store addr)
            0xfd, 0x0c, 10, 0, 0, 0, 20, 0, 0, 0, 30, 0, 0, 0, 40, 0, 0, 0, // v128.const
            0xfd, 0x0b, 0x00, 0x00, // v128.store align0 off0
            0x41, 0x00, // i32.const 0 (load addr)
            0xfd, 0x00, 0x00, 0x00, // v128.load align0 off0
            0xfd, 0x1b, 0x02, // i32x4.extract_lane 2
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]), // memory: min 1
            (7, vec![0x01, 0x01, b's', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "s", &[]).unwrap()[0]), 30);
    }

    #[test]
    fn simd_v128_struct_field() {
        // type 1 = (struct (field (mut v128))); store a v128 (i32x4 lane0=7), read it, extract -> 7
        let mut entry = vec![
            0x01, 0x01, 0x63, 0x01, // 1 local (ref null 1)
            0xfb, 0x01, 0x01, // struct.new_default 1
            0x21, 0x00, // local.set 0
            0x20, 0x00, // local.get 0
            0xfd, 0x0c, // v128.const
        ];
        entry.extend_from_slice(&[7, 0, 0, 0]);
        entry.extend_from_slice(&[0; 12]);
        entry.extend_from_slice(&[
            0xfb, 0x05, 0x01, 0x00, // struct.set type1 field0
            0x20, 0x00, // local.get 0
            0xfb, 0x02, 0x01, 0x00, // struct.get type1 field0
            0xfd, 0x1b, 0x00, // i32x4.extract_lane 0
            0x0b,
        ]);
        let m = asm(&[
            (
                1,
                vec![
                    0x02, // 2 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: ()->i32
                    0x5f, 0x01, 0x7b, 0x01, // type 1: struct { (mut v128) }
                ],
            ),
            (3, vec![0x01, 0x00]),
            (7, vec![0x01, 0x01, b'v', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "v", &[]).unwrap()[0]), 7);
    }

    // --- multi-memory ---

    #[test]
    fn multimem_distinct_routing() {
        // 2 memories; store 7->mem0[0], 9->mem1[0]; (load mem0)*10 + (load mem1) = 79
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x07, 0x36, 0x00, 0x00, // i32.store mem0[0] = 7
            0x41, 0x00, 0x41, 0x09, 0x36, 0x40, 0x01, 0x00, // i32.store mem1[0] = 9 (memidx flag)
            0x41, 0x00, 0x28, 0x00, 0x00, // i32.load mem0[0] -> 7
            0x41, 0x0a, 0x6c, // * 10 -> 70
            0x41, 0x00, 0x28, 0x40, 0x01, 0x00, // i32.load mem1[0] -> 9
            0x6a, // + -> 79
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x02, 0x00, 0x01, 0x00, 0x01]), // 2 memories, each min 1
            (7, vec![0x01, 0x01, b'm', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "m", &[]).unwrap()[0]), 79);
    }

    #[test]
    fn multimem_active_data_to_mem1() {
        // active data segment (flag 2) targets memory 1; load mem1[0] -> 42
        let entry = [0x00, 0x41, 0x00, 0x28, 0x40, 0x01, 0x00, 0x0b]; // i32.load8-less: i32.load mem1[0]
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x02, 0x00, 0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'm', 0x00, 0x00]),
            (10, code1(&entry)),
            // data: 1 segment, flag 2 (active + explicit memidx), memidx 1, (i32.const 0), bytes [42,0,0,0]
            (11, vec![0x01, 0x02, 0x01, 0x41, 0x00, 0x0b, 0x04, 0x2a, 0x00, 0x00, 0x00]),
        ]);
        assert_eq!(as_i32(run1(&m, "m", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn multimem_cross_copy() {
        // store 55 -> mem0[0]; memory.copy dst=mem1 src=mem0 (4 bytes); load mem1[0] -> 55
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x37, 0x36, 0x00, 0x00, // i32.store mem0[0] = 55
            0x41, 0x00, 0x41, 0x00, 0x41, 0x04, 0xfc, 0x0a, 0x01, 0x00, // memory.copy dst=1 src=0
            0x41, 0x00, 0x28, 0x40, 0x01, 0x00, // i32.load mem1[0]
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x02, 0x00, 0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'm', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "m", &[]).unwrap()[0]), 55);
    }

    // --- threads / atomics (0xFE) ---

    /// (memory 1) with a single-memory helper.
    fn mem_func(entry: &[u8], mem_section: Vec<u8>, results: &[u8]) -> Vec<u8> {
        let mut ty = vec![0x01u8, 0x60, 0x00];
        ty.push(results.len() as u8);
        ty.extend_from_slice(results);
        asm(&[
            (1, ty),
            (3, vec![0x01, 0x00]),
            (5, mem_section),
            (7, vec![0x01, 0x01, b'a', 0x00, 0x00]),
            (10, code1(entry)),
        ])
    }

    #[test]
    fn atomic_rmw_add() {
        // store 10 @0; (rmw.add @0, 5) -> old 10; (atomic.load @0) -> 15; 10+15 = 25
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x0a, 0x36, 0x02, 0x00, // i32.store [0] = 10
            0x41, 0x00, 0x41, 0x05, 0xfe, 0x1e, 0x02, 0x00, // i32.atomic.rmw.add [0] 5 -> 10
            0x41, 0x00, 0xfe, 0x10, 0x02, 0x00, // i32.atomic.load [0] -> 15
            0x6a, // + -> 25
            0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 25);
    }

    #[test]
    fn atomic_cmpxchg() {
        // store 7 @0; cmpxchg(@0, expect 7, repl 42) -> old 7; load @0 -> 42; 7+42 = 49
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x07, 0x36, 0x02, 0x00, // i32.store [0] = 7
            0x41, 0x00, 0x41, 0x07, 0x41, 0x2a, 0xfe, 0x48, 0x02, 0x00, // cmpxchg(7 -> 42) -> 7
            0x41, 0x00, 0xfe, 0x10, 0x02, 0x00, // i32.atomic.load [0] -> 42
            0x6a, // + -> 49
            0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 49);
    }

    #[test]
    fn atomic_unaligned_traps() {
        // i32.atomic.load at addr 1 (not 4-aligned) -> UnalignedAtomic
        let entry = [0x00, 0x41, 0x01, 0xfe, 0x10, 0x02, 0x00, 0x0b];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::UnalignedAtomic));
    }

    #[test]
    fn atomic_wait_nonshared_traps() {
        // memory.atomic.wait32 on a non-shared memory -> ExpectedSharedMemory
        let entry = [
            0x00, 0x41, 0x00, 0x41, 0x00, 0x42, 0x00, 0xfe, 0x01, 0x02, 0x00, 0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::ExpectedSharedMemory));
    }

    #[test]
    fn atomic_wait_shared_mismatch() {
        // shared memory (flags 0x03: shared+max); wait32(@0, expect 5, timeout 0);
        // mem[0]=0 != 5 -> returns 1 ("not equal")
        let entry = [
            0x00, 0x41, 0x00, 0x41, 0x05, 0x42, 0x00, 0xfe, 0x01, 0x02, 0x00, 0x0b,
        ];
        let m = mem_func(&entry, vec![0x01, 0x03, 0x01, 0x01], &[0x7f]); // shared, min 1, max 1
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 1);
    }

    #[test]
    fn simd_v128_global() {
        // (global v128 (v128.const i32x4 9 0 0 0)); extract_lane 0 (global.get 0) -> 9
        let mut global = vec![0x01, 0x7b, 0x00, 0xfd, 0x0c]; // 1 global: v128, immutable, v128.const
        global.extend_from_slice(&[9, 0, 0, 0]);
        global.extend_from_slice(&[0; 12]);
        global.push(0x0b); // end of const-expr
        let entry = [0x00, 0x23, 0x00, 0xfd, 0x1b, 0x00, 0x0b]; // global.get 0; i32x4.extract_lane 0
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (6, global),
            (7, vec![0x01, 0x01, b'g', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "g", &[]).unwrap()[0]), 9);
    }

    // --- memory64 ---

    /// A single memory64 memory (limits flag `0x04` = i64 index), min 1 page, no max.
    const MEM64_SECTION: [u8; 3] = [0x01, 0x04, 0x01];

    #[test]
    fn mem64_store_load_roundtrip() {
        // i64.const 8 ; i32.const 1234 ; i32.store ; i64.const 8 ; i32.load
        let entry = [
            0x00, //
            0x42, 0x08, // i64.const 8   (address — i64 because the memory is 64-bit)
            0x41, 0xd2, 0x09, // i32.const 1234
            0x36, 0x02, 0x00, // i32.store align=2 offset=0
            0x42, 0x08, // i64.const 8
            0x28, 0x02, 0x00, // i32.load align=2 offset=0
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 1234);
    }

    #[test]
    fn mem64_size_and_grow_are_i64() {
        // memory.size (1) + memory.grow(2) (old 1) + memory.size (3) = 5, via i64.add.
        let entry = [
            0x00, //
            0x3f, 0x00, // memory.size -> i64 1
            0x42, 0x02, // i64.const 2  (delta — i64)
            0x40, 0x00, // memory.grow -> i64 1 (old page count)
            0x7c, // i64.add -> 2
            0x3f, 0x00, // memory.size -> i64 3
            0x7c, // i64.add -> 5
            0xa7, // i32.wrap_i64
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 5);
    }

    #[test]
    fn mem64_grow_beyond_max_returns_minus_one() {
        // (memory i64 1 1) — grow by 1 exceeds the declared max, so grow yields i64 -1.
        let entry = [
            0x00, //
            0x42, 0x01, // i64.const 1
            0x40, 0x00, // memory.grow -> i64 -1 (refused)
            0xa7, // i32.wrap_i64
            0x0b,
        ];
        // flag 0x05 = has-max + i64 index; min 1, max 1.
        let m = mem_func(&entry, vec![0x01, 0x05, 0x01, 0x01], &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), -1);
    }

    #[test]
    fn mem64_active_data_segment_uses_i64_offset() {
        // active data segment whose offset const-expr is `i64.const 4` (the memory's index type)
        let entry = [0x00, 0x42, 0x04, 0x28, 0x02, 0x00, 0x0b]; // i64.const 4 ; i32.load
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, MEM64_SECTION.to_vec()),
            (7, vec![0x01, 0x01, b'a', 0x00, 0x00]),
            (10, code1(&entry)),
            // 1 segment, flag 0 (active, memory 0), (i64.const 4), 4 bytes = 42 LE
            (11, vec![0x01, 0x00, 0x42, 0x04, 0x0b, 0x04, 0x2a, 0x00, 0x00, 0x00]),
        ]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn mem64_address_above_4gib_traps() {
        // The memory64-defining case: an address of 2^32 must trap, NOT wrap to 0 the way a
        // 32-bit address would. Proves the address is carried full-width.
        let entry = [
            0x00, //
            0x42, 0x80, 0x80, 0x80, 0x80, 0x10, // i64.const 0x1_0000_0000
            0x28, 0x02, 0x00, // i32.load
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::MemoryOutOfBounds));
    }

    #[test]
    fn mem64_memarg_offset_above_u32() {
        // A 64-bit memory may carry a static offset wider than u32 (decoded as a full u64
        // LEB). Address 0 + offset 2^32 is out of bounds -> trap, not a decode error.
        let entry = [
            0x00, //
            0x42, 0x00, // i64.const 0
            0x28, 0x02, 0x80, 0x80, 0x80, 0x80, 0x10, // i32.load offset=0x1_0000_0000
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::MemoryOutOfBounds));
    }

    #[test]
    fn mem64_bulk_fill_and_copy() {
        // fill mem[0..4] = 0xab ; copy mem[0..4] -> mem[16..20] ; load mem[16]
        let entry = [
            0x00, //
            0x42, 0x00, // i64.const 0    (dst)
            0x41, 0xab, 0x01, // i32.const 0xab (byte — always i32)
            0x42, 0x04, // i64.const 4    (n — i64 on a 64-bit memory)
            0xfc, 0x0b, 0x00, // memory.fill 0
            0x42, 0x10, // i64.const 16   (dst)
            0x42, 0x00, // i64.const 0    (src)
            0x42, 0x04, // i64.const 4    (n — i64: both memories are 64-bit)
            0xfc, 0x0a, 0x00, 0x00, // memory.copy dst=0 src=0
            0x42, 0x10, // i64.const 16
            0x28, 0x02, 0x00, // i32.load
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        let got = as_i32(run1(&m, "a", &[]).unwrap()[0]) as u32;
        assert_eq!(got, 0xabab_abab);
    }

    #[test]
    fn mem64_memory_init_dst_is_i64() {
        // memory.init: n/src index the segment (i32), but dst is the memory's index type.
        let entry = [
            0x00, //
            0x42, 0x20, // i64.const 32 (dst — i64)
            0x41, 0x00, // i32.const 0  (src into the segment)
            0x41, 0x04, // i32.const 4  (n)
            0xfc, 0x08, 0x00, 0x00, // memory.init data=0 mem=0
            0x42, 0x20, // i64.const 32
            0x28, 0x02, 0x00, // i32.load
            0x0b,
        ];
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            (3, vec![0x01, 0x00]),
            (5, MEM64_SECTION.to_vec()),
            (7, vec![0x01, 0x01, b'a', 0x00, 0x00]),
            (12, vec![0x01]), // data count = 1
            (10, code1(&entry)),
            // 1 passive segment (flag 1), 4 bytes = 99 LE
            (11, vec![0x01, 0x01, 0x04, 0x63, 0x00, 0x00, 0x00]),
        ]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 99);
    }

    #[test]
    fn mem64_atomic_address_is_i64() {
        // The 0xFE family routes its address through the same 64-bit path.
        let entry = [
            0x00, //
            0x42, 0x00, // i64.const 0
            0x41, 0x0a, // i32.const 10
            0x36, 0x02, 0x00, // i32.store [0] = 10
            0x42, 0x00, // i64.const 0
            0x41, 0x05, // i32.const 5
            0xfe, 0x1e, 0x02, 0x00, // i32.atomic.rmw.add -> old 10
            0x42, 0x00, // i64.const 0
            0xfe, 0x10, 0x02, 0x00, // i32.atomic.load -> 15
            0x6a, // + -> 25
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 25);
    }

    #[test]
    fn mem64_huge_minimum_exceeds_the_budget() {
        // A 64-bit memory may DECLARE up to 2^48 pages; this instance still refuses to back
        // more than its budget, and must do so without overflowing the size computation.
        let entry = [0x00, 0x3f, 0x00, 0xa7, 0x0b]; // memory.size ; i32.wrap_i64
        // min = 2^40 pages (64 TiB)
        let mem = vec![0x01, 0x04, 0x80, 0x80, 0x80, 0x80, 0x80, 0x20];
        let m = mem_func(&entry, mem, &[0x7f]);
        assert_eq!(run1(&m, "a", &[]), Err(Trap::MemoryLimitExceeded));
    }

    #[test]
    fn mem64_cross_copy_with_a_32bit_memory() {
        // memory.copy between a 64-bit dst and a 32-bit src: each address keeps its own
        // index type, and the count is i32 (the narrower of the two).
        let entry = [
            0x00, //
            0x41, 0x00, 0x41, 0x37, 0x36, 0x40, 0x01, 0x00, // i32.store mem1[0] = 55
            0x42, 0x00, // i64.const 0  (dst — mem0 is 64-bit)
            0x41, 0x00, // i32.const 0  (src — mem1 is 32-bit)
            0x41, 0x04, // i32.const 4  (n — i32, not both memories are 64-bit)
            0xfc, 0x0a, 0x00, 0x01, // memory.copy dst=0 src=1
            0x42, 0x00, 0x28, 0x02, 0x00, // i64.const 0 ; i32.load mem0[0]
            0x0b,
        ];
        // 2 memories: mem0 = i64-indexed min 1, mem1 = 32-bit min 1.
        let m = mem_func(&entry, vec![0x02, 0x04, 0x01, 0x00, 0x01], &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 55);
    }

    #[test]
    fn mem64_simd_address_is_i64() {
        // v128 loads/stores share `simd_mem_ea`, which is memory64-aware.
        let entry = [
            0x00, //
            0x42, 0x00, // i64.const 0
            0x41, 0x07, // i32.const 7
            0xfd, 0x11, // i32x4.splat
            0xfd, 0x0b, 0x04, 0x00, // v128.store align=4 offset=0
            0x42, 0x00, // i64.const 0
            0xfd, 0x00, 0x04, 0x00, // v128.load align=4 offset=0
            0xfd, 0x1b, 0x02, // i32x4.extract_lane 2
            0x0b,
        ];
        let m = mem_func(&entry, MEM64_SECTION.to_vec(), &[0x7f]);
        assert_eq!(as_i32(run1(&m, "a", &[]).unwrap()[0]), 7);
    }

    // --- exception handling ---

    /// An EH module: type 0 = `() -> results`, type 1 = `(i32) -> ()` (the tag's type),
    /// one tag of type 1, one exported function `e` with `entry` as its body.
    fn eh_func(entry: &[u8], results: &[u8]) -> Vec<u8> {
        let mut ty = vec![0x02u8, 0x60, 0x00];
        ty.push(results.len() as u8);
        ty.extend_from_slice(results);
        ty.extend_from_slice(&[0x60, 0x01, 0x7f, 0x00]); // type 1: (i32) -> ()
        asm(&[
            (1, ty),
            (3, vec![0x01, 0x00]),
            (13, vec![0x01, 0x00, 0x01]), // tag section: 1 tag, attribute 0, type 1
            (7, vec![0x01, 0x01, b'e', 0x00, 0x00]),
            (10, code1(entry)),
        ])
    }

    #[test]
    fn eh_try_table_catches_throw() {
        // (block (result i32)
        //   (try_table (result i32) (catch $e 0)   ;; label 0 = the enclosing block —
        //                                          ;; a catch label counts from OUTSIDE the
        //                                          ;; try_table (§ `C ⊢ catch ok`).
        //     i32.const 42 ; throw $e))            ;; the payload lands in the block
        let entry = [
            0x00, //
            0x02, 0x7f, // block (result i32)
            0x1f, 0x7f, 0x01, 0x00, 0x00, 0x00, // try_table (result i32) [catch tag 0 -> label 0]
            0x41, 0x2a, // i32.const 42
            0x08, 0x00, // throw tag 0
            0x0b, // end try_table
            0x0b, // end block
            0x0b, // end func
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn eh_uncaught_throw_traps() {
        // A throw with no enclosing handler escapes the invocation.
        let entry = [0x00, 0x41, 0x07, 0x08, 0x00, 0x0b];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(run1(&m, "e", &[]), Err(Trap::UncaughtException));
    }

    #[test]
    fn eh_catch_all_binds_nothing() {
        // `catch_all` matches any tag and binds NO payload, so its target label must be
        // void — the value has to come from elsewhere. A local records which path ran:
        // the handler branches straight out of the block, skipping the `99`.
        let entry = [
            0x01, 0x01, 0x7f, // one i32 local
            0x41, 0x07, 0x21, 0x00, // local 0 = 7
            0x02, 0x40, // block (void)
            0x1f, 0x40, 0x01, 0x02, 0x00, // try_table (void) [catch_all -> label 0 = the block]
            0x41, 0x05, // i32.const 5
            0x08, 0x00, // throw tag 0
            0x0b, // end try_table
            0x41, 0x63, 0x21, 0x00, // local 0 = 99  (normal completion only)
            0x0b, // end block
            0x20, 0x00, // local.get 0
            0x0b,
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 7);
    }

    #[test]
    fn eh_exception_unwinds_across_a_call() {
        // f0 catches what f1 throws — the exception crosses a call boundary.
        // f1: i32.const 8 ; throw $e      (uncaught in f1)
        // f0: block (result i32) (try_table (catch $e 0) (call 1)) end
        let f0 = [
            0x00, //
            0x02, 0x7f, // block (result i32)
            0x1f, 0x40, 0x01, 0x00, 0x00, 0x00, // try_table (void) [catch tag 0 -> label 0]
            0x10, 0x01, // call 1
            0x0b, // end try_table
            0x41, 0x00, // i32.const 0 (normal path)
            0x0b, // end block
            0x0b,
        ];
        let f1 = [0x00, 0x41, 0x08, 0x08, 0x00, 0x0b]; // i32.const 8 ; throw $e
        let m = asm(&[
            (
                1,
                vec![
                    0x03, // 3 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: () -> i32
                    0x60, 0x01, 0x7f, 0x00, // type 1: (i32) -> ()   (the tag)
                    0x60, 0x00, 0x00, // type 2: () -> ()
                ],
            ),
            (3, vec![0x02, 0x00, 0x02]), // func 0: type 0, func 1: type 2
            (13, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'e', 0x00, 0x00]),
            (10, code_n(&[&f0, &f1])),
        ]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 8);
    }

    #[test]
    fn eh_catch_ref_and_throw_ref() {
        // The inner try_table catches BY REFERENCE (kind 0x01): its target label receives the
        // tag's payload *plus* an `exnref`. `throw_ref` then re-raises that boxed exception
        // for the outer try_table to catch by value — proving the box round-trips its payload.
        let entry = [
            0x00, //
            0x02, 0x7f, // block $outer (result i32)
            0x1f, 0x40, 0x01, 0x00, 0x00, 0x00, // try_table (void) [catch tag0 -> $outer]
            0x02, 0x02, // block $inner : type 2 = () -> (i32, exnref)
            0x1f, 0x40, 0x01, 0x01, 0x00, 0x00, // try_table (void) [catch_ref tag0 -> $inner]
            0x41, 0x11, // i32.const 17
            0x08, 0x00, // throw tag 0
            0x0b, // end inner try_table
            0x00, // unreachable (the try_table's normal-completion path)
            0x0b, // end $inner  -> stack: [17, exnref]
            0x0a, // throw_ref   -> re-raise the boxed exception
            0x0b, // end outer try_table
            0x41, 0x00, // i32.const 0 (normal path)
            0x0b, // end $outer
            0x0b,
        ];
        let m = asm(&[
            (
                1,
                vec![
                    0x03, // 3 types
                    0x60, 0x00, 0x01, 0x7f, // type 0: () -> i32
                    0x60, 0x01, 0x7f, 0x00, // type 1: (i32) -> ()  (the tag)
                    0x60, 0x00, 0x02, 0x7f, 0x69, // type 2: () -> (i32, exnref)
                ],
            ),
            (3, vec![0x01, 0x00]),
            (13, vec![0x01, 0x00, 0x01]),
            (7, vec![0x01, 0x01, b'e', 0x00, 0x00]),
            (10, code1(&entry)),
        ]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 17);
    }

    #[test]
    fn eh_legacy_try_catch() {
        // The legacy encoding: (try (result i32) (i32.const 3) (throw $e) (catch $e))
        // The handler runs INSIDE the try and binds the payload.
        let entry = [
            0x00, //
            0x06, 0x7f, // try (result i32)
            0x41, 0x03, // i32.const 3
            0x08, 0x00, // throw tag 0
            0x07, 0x00, // catch tag 0   -> handler binds the i32 payload
            0x0b, // end try
            0x0b,
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 3);
    }

    #[test]
    fn eh_legacy_catch_all() {
        // Legacy catch_all (0x19) binds nothing, so the handler supplies the result.
        let entry = [
            0x00, //
            0x06, 0x7f, // try (result i32)
            0x41, 0x01, // i32.const 1
            0x08, 0x00, // throw tag 0
            0x19, // catch_all
            0x41, 0x37, // i32.const 55
            0x0b, // end try
            0x0b,
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 55);
    }

    #[test]
    fn eh_legacy_rethrow_propagates_outward() {
        // An inner legacy try catches, then `rethrow 0` re-raises the caught exception from
        // OUTSIDE that try, so the outer try's handler is the one that finally binds it.
        let entry = [
            0x00, //
            0x06, 0x7f, // try $outer (result i32)
            0x06, 0x40, // try $inner (void)
            0x41, 0x09, // i32.const 9
            0x08, 0x00, // throw tag 0
            0x19, // catch_all ($inner)
            0x09, 0x00, // rethrow 0  -> re-raise from outside $inner
            0x0b, // end $inner
            0x41, 0x00, // i32.const 0 (normal path of $outer's body)
            0x07, 0x00, // catch tag 0 ($outer) -> binds the payload 9
            0x0b, // end $outer
            0x0b,
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(as_i32(run1(&m, "e", &[]).unwrap()[0]), 9);
    }

    #[test]
    fn eh_legacy_throw_from_handler_escapes_its_own_try() {
        // The re-throw idiom `catch (e) { throw e; }` must propagate OUTWARD, not re-match
        // the handler it is already inside (which would loop forever).
        let entry = [
            0x00, //
            0x06, 0x40, // try (void)
            0x41, 0x02, // i32.const 2
            0x08, 0x00, // throw tag 0
            0x07, 0x00, // catch tag 0 -> handler; payload on the stack
            0x08, 0x00, // throw tag 0 again, from inside the handler
            0x0b, // end try
            0x41, 0x00, // (unreachable)
            0x0b,
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(run1(&m, "e", &[]), Err(Trap::UncaughtException));
    }

    #[test]
    fn eh_legacy_delegate_traps_while_unwinding() {
        // `delegate` re-raises "at label l", routing the frozen oracle does not implement
        // (and its validator rejects). Reaching one while unwinding must trap loudly rather
        // than silently mis-route.
        let entry = [
            0x00, //
            0x02, 0x40, // block
            0x06, 0x40, // try
            0x41, 0x04, // i32.const 4
            0x08, 0x00, // throw tag 0
            0x18, 0x00, // delegate 0  (terminates the try)
            0x0b, // end block
            0x41, 0x00, //
            0x0b,
        ];
        let m = eh_func(&entry, &[0x7f]);
        assert_eq!(run1(&m, "e", &[]), Err(Trap::UnsupportedInstruction));
    }

    #[test]
    fn eh_state_does_not_leak_between_invocations() {
        // An escaping exception must not be visible to the next call on the same instance.
        let entry = [0x00, 0x41, 0x07, 0x08, 0x00, 0x0b];
        let m = eh_func(&entry, &[0x7f]);
        let md = decode(&m).unwrap();
        let mut inst = Instance::new(md).unwrap();
        assert_eq!(inst.invoke("e", &[]), Err(Trap::UncaughtException));
        assert_eq!(inst.invoke("e", &[]), Err(Trap::UncaughtException));
    }

    // --- host imports (T7a) ---

    /// A module importing `(func (param i32 i32) (result i32))` from `"env" "h"`, exporting
    /// `call_host` which forwards its two arguments.
    fn host_import_module() -> Vec<u8> {
        asm(&[
            (
                1,
                vec![
                    0x01, // 1 type
                    0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // (i32 i32) -> i32
                ],
            ),
            // import: "env" "h" func type 0
            (
                2,
                vec![
                    0x01, 0x03, b'e', b'n', b'v', 0x01, b'h', 0x00, 0x00,
                ],
            ),
            (3, vec![0x01, 0x00]), // one defined func, type 0
            (7, vec![0x01, 0x01, b'c', 0x00, 0x01]), // export "c" = func 1 (the DEFINED one)
            // body: local.get 0 ; local.get 1 ; call 0  (func 0 is the import)
            (10, code1(&[0x00, 0x20, 0x00, 0x20, 0x01, 0x10, 0x00, 0x0b])),
        ])
    }

    #[test]
    fn calls_an_imported_host_function() {
        let md = decode(&host_import_module()).unwrap();
        let imports = Imports::new().with_func(|_caller, args, results| {
            results[0] = i32_value(as_i32(args[0]) * as_i32(args[1]));
            Ok(())
        });
        let mut inst = Instance::new_with_imports(md, imports).unwrap();
        let r = inst
            .invoke("c", &[i32_value(6), i32_value(7)])
            .unwrap();
        assert_eq!(as_i32(r[0]), 42);
    }

    #[test]
    fn a_host_function_can_trap_the_guest() {
        let md = decode(&host_import_module()).unwrap();
        let imports = Imports::new().with_func(|_c, _a, _r| Err(Trap::HostTrap));
        let mut inst = Instance::new_with_imports(md, imports).unwrap();
        assert_eq!(
            inst.invoke("c", &[i32_value(1), i32_value(2)]),
            Err(Trap::HostTrap)
        );
    }

    #[test]
    fn the_import_count_must_match() {
        // Too few: the module declares one function import, the host supplies none.
        let md = decode(&host_import_module()).unwrap();
        assert_eq!(Instance::new(md).err(), Some(Trap::MissingImport));
        // Too many is equally wrong — a silent extra would mis-align every later index.
        let md = decode(&host_import_module()).unwrap();
        let two = Imports::new()
            .with_func(|_c, _a, _r| Ok(()))
            .with_func(|_c, _a, _r| Ok(()));
        assert_eq!(
            Instance::new_with_imports(md, two).err(),
            Some(Trap::MissingImport)
        );
    }

    #[test]
    fn an_import_shifts_the_defined_function_indices() {
        // The regression this design invites: with one import, defined function 0 lives at
        // function-space index 1. Calling `c` proves the offset is applied — before the
        // fix, `call 0` would have re-entered the exported function instead of the host.
        let md = decode(&host_import_module()).unwrap();
        let calls = core::cell::Cell::new(0u32);
        // The closure borrows nothing outside itself, so count via an owned cell.
        let imports = Imports::new().with_func(move |_c, args, results| {
            calls.set(calls.get() + 1);
            results[0] = i32_value(as_i32(args[0]) + as_i32(args[1]) + calls.get() as i32);
            Ok(())
        });
        let mut inst = Instance::new_with_imports(md, imports).unwrap();
        assert_eq!(
            as_i32(inst.invoke("c", &[i32_value(10), i32_value(1)]).unwrap()[0]),
            12 // 10 + 1 + first call
        );
        assert_eq!(
            as_i32(inst.invoke("c", &[i32_value(10), i32_value(1)]).unwrap()[0]),
            13 // the same host closure, called a second time
        );
    }

    #[test]
    fn a_host_function_reads_and_writes_guest_memory() {
        // How every WASI call works: the guest passes a pointer, the host reads/writes it.
        let m = asm(&[
            (1, vec![0x02, 0x60, 0x01, 0x7f, 0x00, 0x60, 0x00, 0x01, 0x7f]),
            (
                2,
                vec![0x01, 0x03, b'e', b'n', b'v', 0x01, b'h', 0x00, 0x00],
            ),
            (3, vec![0x01, 0x01]), // defined func of type 1: () -> i32
            (5, vec![0x01, 0x00, 0x01]), // one memory, min 1
            (7, vec![0x01, 0x01, b'c', 0x00, 0x01]),
            // i32.const 16 ; call 0 (host writes at 16) ; i32.load offset=16
            (
                10,
                code1(&[
                    0x00, 0x41, 0x10, 0x10, 0x00, 0x41, 0x00, 0x28, 0x02, 0x10, 0x0b,
                ]),
            ),
        ]);
        let md = decode(&m).unwrap();
        let imports = Imports::new().with_func(|caller: &mut Caller<'_>, args, _r| {
            let addr = as_i32(args[0]) as u64;
            let dst = caller.write(0, addr, 4).ok_or(Trap::MemoryOutOfBounds)?;
            dst.copy_from_slice(&1234i32.to_le_bytes());
            Ok(())
        });
        let mut inst = Instance::new_with_imports(md, imports).unwrap();
        assert_eq!(as_i32(inst.invoke("c", &[]).unwrap()[0]), 1234);
    }

    #[test]
    fn an_out_of_bounds_host_access_is_rejected_not_panicked() {
        // A host function handed a bad guest pointer must get `None`, not a panic — the
        // library-must-not-abort-its-embedder rule.
        let md = decode(&host_import_module()).unwrap();
        let imports = Imports::new().with_func(|caller: &mut Caller<'_>, _a, r| {
            // No memory at all in this module, and a wild address.
            assert!(caller.read(0, u64::MAX, 8).is_none());
            assert!(caller.write(7, 0, 1).is_none());
            assert!(caller.memory_len(0).is_none());
            r[0] = i32_value(0);
            Ok(())
        });
        let mut inst = Instance::new_with_imports(md, imports).unwrap();
        assert_eq!(as_i32(inst.invoke("c", &[i32_value(0), i32_value(0)]).unwrap()[0]), 0);
    }

    #[test]
    fn imported_globals_are_visible_to_defined_initializers() {
        // Global 0 is imported; the defined global 1 initializes from it.
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x01, 0x7f]),
            // import "env" "g" (global i32, immutable)
            (
                2,
                vec![0x01, 0x03, b'e', b'n', b'v', 0x01, b'g', 0x03, 0x7f, 0x00],
            ),
            (3, vec![0x01, 0x00]),
            // defined global 1 : i32 = global.get 0
            (6, vec![0x01, 0x7f, 0x00, 0x23, 0x00, 0x0b]),
            (7, vec![0x01, 0x01, b'c', 0x00, 0x00]),
            (10, code1(&[0x00, 0x23, 0x01, 0x0b])), // global.get 1
        ]);
        let md = decode(&m).unwrap();
        let imports = Imports::new().with_global(i32_value(99));
        let mut inst = Instance::new_with_imports(md, imports).unwrap();
        assert_eq!(as_i32(inst.invoke("c", &[]).unwrap()[0]), 99);
    }

    #[test]
    fn a_lone_instance_cannot_satisfy_a_memory_import() {
        // A memory import needs another instance to share from, so there is nothing a
        // single-instance `Instance::new` can bind it to. It must fail visibly rather than
        // quietly allocating a private memory the exporter cannot see.
        let m = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x00]),
            (
                2,
                vec![0x01, 0x03, b'e', b'n', b'v', 0x01, b'm', 0x02, 0x00, 0x01],
            ),
            (3, vec![0x01, 0x00]),
            (10, code1(&[0x00, 0x0b])),
        ]);
        let md = decode(&m).unwrap();
        assert_eq!(Instance::new(md).err(), Some(Trap::MissingImport));
    }

    #[test]
    fn a_lone_instance_cannot_satisfy_a_table_import() {
        // Like a memory import: a table import needs another instance to share from, so a
        // single-instance `Instance::new` has nothing to bind it to.
        let m = crate::wat::assemble(
            br#"(module (import "env" "t" (table 1 funcref)) (func (export "f")))"#,
        )
        .unwrap();
        let md = decode(&m).unwrap();
        assert_eq!(Instance::new(md).err(), Some(Trap::MissingImport));
    }

    /// **The defect the funcref encoding exists to prevent.** Two instances share one table; the
    /// exporter stores `ref.func` of *its* function 0 into it, and the importer calls slot 0 through
    /// `call_indirect`. It must reach the EXPORTER's function.
    ///
    /// Both modules define a function at index 0 returning a different value, so the wrong answer and
    /// the right one are distinguishable — the two-instance rule applied to the value model. Before a
    /// funcref carried its owner this was a silent wrong call, which is why imported tables were
    /// refused outright rather than shipped.
    #[test]
    fn a_shared_table_dispatches_to_the_funcrefs_own_instance() {
        let mut store = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let provider = store
            .instantiate(
                mk(br#"(module
                        (table (export "t") 1 funcref)
                        (func $mine (result i32) (i32.const 0x11))
                        (func (export "fill") (table.set (i32.const 0) (ref.func $mine)))
                        (elem declare func $mine))"#),
                Imports::new(),
            )
            .unwrap();
        let ti = store
            .export_index(provider, "t", crate::types::ExternKind::Table)
            .unwrap();
        store.invoke(provider, "fill", &[]).unwrap();
        let consumer = store
            .instantiate(
                mk(br#"(module
                        (import "p" "t" (table 1 funcref))
                        (type $sig (func (result i32)))
                        (func $decoy (result i32) (i32.const 0x99))
                        (func (export "go") (result i32)
                          (call_indirect (type $sig) (i32.const 0))))"#),
                Imports::new().with_instance_table(provider, ti),
            )
            .unwrap();
        // 0x11 = the provider's function. 0x99 would mean the entry was resolved against the
        // *calling* instance — the silent wrong call this encoding removes.
        assert_eq!(as_i32(store.invoke(consumer, "go", &[]).unwrap()[0]), 0x11);
    }

    /// §4.5.9 table matching: the element type must be **equal** (a table is mutable, so a narrower
    /// actual type would let the importer write what the exporter's type forbids) and the limits must
    /// satisfy the declaration.
    #[test]
    fn a_table_import_whose_type_does_not_match_is_refused() {
        let mut store = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let provider = store
            .instantiate(mk(br#"(module (table (export "t") 1 funcref))"#), Imports::new())
            .unwrap();
        let ti = store
            .export_index(provider, "t", crate::types::ExternKind::Table)
            .unwrap();
        // Wrong element type.
        assert_eq!(
            store
                .instantiate(
                    mk(br#"(module (import "p" "t" (table 1 externref)))"#),
                    Imports::new().with_instance_table(provider, ti),
                )
                .err(),
            Some(Trap::IncompatibleImport)
        );
        // Declared minimum larger than the actual.
        assert_eq!(
            store
                .instantiate(
                    mk(br#"(module (import "p" "t" (table 2 funcref)))"#),
                    Imports::new().with_instance_table(provider, ti),
                )
                .err(),
            Some(Trap::IncompatibleImport)
        );
        // An unbounded table does not satisfy a bounded import.
        assert_eq!(
            store
                .instantiate(
                    mk(br#"(module (import "p" "t" (table 1 4 funcref)))"#),
                    Imports::new().with_instance_table(provider, ti),
                )
                .err(),
            Some(Trap::IncompatibleImport)
        );
        // The matching declaration links.
        assert!(
            store
                .instantiate(
                    mk(br#"(module (import "p" "t" (table 1 funcref)))"#),
                    Imports::new().with_instance_table(provider, ti),
                )
                .is_ok()
        );
    }

    // --- imported memories (T9a#4, the memory half) ---

    /// Two instances, one memory: a write through the *importer* must be visible to the
    /// *exporter*. Copying the bytes at link time would pass a one-instance test and fail this.
    #[test]
    fn an_imported_memory_is_the_same_memory() {
        let mut store = Store::new();
        let provider = store
            .instantiate(
                decode(
                    &crate::wat::assemble(
                        br#"(module (memory (export "m") 1)
                             (func (export "peek") (result i32) (i32.load (i32.const 4))))"#,
                    )
                    .unwrap(),
                )
                .unwrap(),
                Imports::new(),
            )
            .unwrap();
        let index = store
            .export_index(provider, "m", crate::types::ExternKind::Memory)
            .unwrap();
        let consumer = store
            .instantiate(
                decode(
                    &crate::wat::assemble(
                        br#"(module (import "env" "m" (memory 1))
                             (func (export "poke") (i32.store (i32.const 4) (i32.const 0x2a))))"#,
                    )
                    .unwrap(),
                )
                .unwrap(),
                Imports::new().with_instance_memory(provider, index),
            )
            .unwrap();
        store.invoke(consumer, "poke", &[]).unwrap();
        // Read it back through the PROVIDER, whose own memory index is its own.
        assert_eq!(as_i32(store.invoke(provider, "peek", &[]).unwrap()[0]), 0x2a);
    }

    /// The shared-store defect class one level down: the importer's memory index 0 must resolve
    /// to the *provider's* slot, not to slot 0 of the pool. Two providers make the two differ —
    /// with one, the wrong answer and the right one coincide.
    #[test]
    fn an_imported_memory_resolves_through_the_maps_not_by_raw_index() {
        let mut store = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        // Slot 0: a decoy holding 0x11 at address 0.
        let decoy = store
            .instantiate(
                mk(br#"(module (memory (export "m") 1) (data (i32.const 0) "\11\00\00\00"))"#),
                Imports::new(),
            )
            .unwrap();
        // Slot 1: the memory actually imported, holding 0x22.
        let real = store
            .instantiate(
                mk(br#"(module (memory (export "m") 1) (data (i32.const 0) "\22\00\00\00"))"#),
                Imports::new(),
            )
            .unwrap();
        let index = store
            .export_index(real, "m", crate::types::ExternKind::Memory)
            .unwrap();
        let consumer = store
            .instantiate(
                mk(br#"(module (import "env" "m" (memory 1))
                        (func (export "read") (result i32) (i32.load (i32.const 0))))"#),
                Imports::new().with_instance_memory(real, index),
            )
            .unwrap();
        assert_eq!(as_i32(store.invoke(consumer, "read", &[]).unwrap()[0]), 0x22);
        let _ = decoy;
    }

    /// An active data segment in the *importer* writes into the imported memory, which lives in
    /// the pools already rather than in the instantiation's local vector — the one place the two
    /// code paths for "which memory" diverge.
    #[test]
    fn an_active_data_segment_targets_the_imported_memory() {
        let mut store = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let provider = store
            .instantiate(
                mk(br#"(module (memory (export "m") 1)
                        (func (export "peek") (result i32) (i32.load (i32.const 8))))"#),
                Imports::new(),
            )
            .unwrap();
        let index = store
            .export_index(provider, "m", crate::types::ExternKind::Memory)
            .unwrap();
        store
            .instantiate(
                mk(br#"(module (import "env" "m" (memory 1)) (data (i32.const 8) "\77\00\00\00"))"#),
                Imports::new().with_instance_memory(provider, index),
            )
            .unwrap();
        assert_eq!(as_i32(store.invoke(provider, "peek", &[]).unwrap()[0]), 0x77);
    }

    /// **Sharing survives a re-export chain.** A defines the memory, B imports and re-exports it,
    /// C imports from B — C must reach *A's* bytes. This is where a naive implementation breaks:
    /// B's exported memory is B's own index 0, which is itself an import, so resolving it means
    /// following B's map rather than allocating for B or reading a slot B never owned.
    #[test]
    fn an_imported_memory_survives_a_reexport_chain() {
        let mut s = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let a = s
            .instantiate(
                mk(br#"(module (memory (export "m") 1) (data (i32.const 0) "\aa\00\00\00"))"#),
                Imports::new(),
            )
            .unwrap();
        let ai = s
            .export_index(a, "m", crate::types::ExternKind::Memory)
            .unwrap();
        let b = s
            .instantiate(
                mk(br#"(module (import "x" "m" (memory 1)) (export "m2" (memory 0)))"#),
                Imports::new().with_instance_memory(a, ai),
            )
            .unwrap();
        let bi = s
            .export_index(b, "m2", crate::types::ExternKind::Memory)
            .unwrap();
        let c = s
            .instantiate(
                mk(br#"(module (import "y" "m" (memory 1))
                        (func (export "r") (result i32) (i32.load (i32.const 0))))"#),
                Imports::new().with_instance_memory(b, bi),
            )
            .unwrap();
        assert_eq!(as_i32(s.invoke(c, "r", &[]).unwrap()[0]), 0xaa);
    }

    /// **Two stores cannot pull from each other.** An [`InstanceId`] issued by store X, handed to
    /// store Y, must be refused — not resolved against Y's own instance vector.
    ///
    /// This was a real defect: the index alone was in range in Y, so Y linked the import and the
    /// guest read **Y's own memory** while believing it shared X's. Silent wrong memory, the class
    /// every serious defect in this port has belonged to. Mutation check: drop the
    /// `id.store == self.id` test in [`Store::slot`] and the first assertion reads `0x99` rather
    /// than failing to link.
    #[test]
    fn an_instance_id_from_another_store_is_refused_not_followed() {
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let mut x = Store::new();
        let ax = x
            .instantiate(
                mk(br#"(module (memory (export "m") 1) (data (i32.const 0) "\11\00\00\00"))"#),
                Imports::new(),
            )
            .unwrap();
        let mut y = Store::new();
        // Y has an instance at the same INDEX holding different bytes — which is what made the old
        // behaviour silent rather than a crash.
        let _decoy = y
            .instantiate(
                mk(br#"(module (memory (export "m") 1) (data (i32.const 0) "\99\00\00\00"))"#),
                Imports::new(),
            )
            .unwrap();
        assert_eq!(
            y.instantiate(
                mk(br#"(module (import "x" "m" (memory 1)))"#),
                Imports::new().with_instance_memory(ax, 0),
            )
            .err(),
            Some(Trap::MissingImport)
        );
        // The same applies to a foreign FUNCTION backing (pre-existing since T7b, same root cause).
        assert_eq!(
            y.instantiate(
                mk(br#"(module (import "x" "f" (func)))"#),
                Imports::new().with_instance_func(ax, 0),
            )
            .err(),
            Some(Trap::MissingImport)
        );
        // And a foreign id no longer panics the accessors — it reports absence. Under
        // `panic = "abort"` the old `code[id]` indexing was a process kill, not an error.
        //
        // The two invoke paths refuse with different traps, because each fails at its own first
        // lookup: by name the export search comes up empty (`UndefinedExport`), by index the slot
        // resolution does (`UndefinedFunc`). Neither is a *diagnosis* of "wrong store" — that would
        // want its own variant — but both refuse, which is the property that matters.
        assert_eq!(
            y.invoke(ax, "anything", &[]).err(),
            Some(Trap::UndefinedExport)
        );
        assert_eq!(y.invoke_index(ax, 0, &[]).err(), Some(Trap::UndefinedFunc));
        assert!(y.module_of(ax).is_none());
        assert!(y.memory(ax, 0).is_none());
        assert!(y.export_func(ax, "m").is_none());
        assert!(y.global(ax, 0).is_none());
        assert!(!y.has_export(ax, "m", crate::types::ExternKind::Memory));
    }

    /// A cycle is **unrepresentable**, not rejected: an `InstanceId` exists only once its instance
    /// does, so B can import from A only after A is built, and A can never name B. There is no test
    /// that builds a cycle and expects failure because one cannot be written — this pins the
    /// property that makes that true.
    #[test]
    fn an_import_can_only_name_an_already_built_instance() {
        let mut s = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let a = s
            .instantiate(mk(br#"(module (memory (export "m") 1))"#), Imports::new())
            .unwrap();
        assert!(s.slot(a).is_some());
        assert_eq!(a.index, 0);
        assert_eq!(s.instance_count(), 1);
        // The id the *next* instance will get does not exist yet and cannot be constructed:
        // `InstanceId`'s fields are private and only `instantiate` issues one.
    }

    /// §4.5.9 limits matching. Importing `(memory 2)` from a memory declared `(memory 1)` must
    /// be refused — the guest would otherwise index pages that do not exist.
    #[test]
    fn a_memory_import_whose_limits_do_not_match_is_refused() {
        let mut store = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let provider = store
            .instantiate(mk(br#"(module (memory (export "m") 1))"#), Imports::new())
            .unwrap();
        let index = store
            .export_index(provider, "m", crate::types::ExternKind::Memory)
            .unwrap();
        assert_eq!(
            store
                .instantiate(
                    mk(br#"(module (import "env" "m" (memory 2)))"#),
                    Imports::new().with_instance_memory(provider, index),
                )
                .err(),
            Some(Trap::IncompatibleImport)
        );
        // An unbounded memory does not satisfy a *bounded* import either: the importer's
        // declared ceiling would not be enforced.
        assert_eq!(
            store
                .instantiate(
                    mk(br#"(module (import "env" "m" (memory 1 4)))"#),
                    Imports::new().with_instance_memory(provider, index),
                )
                .err(),
            Some(Trap::IncompatibleImport)
        );
        // And the same type does match.
        assert!(
            store
                .instantiate(
                    mk(br#"(module (import "env" "m" (memory 1)))"#),
                    Imports::new().with_instance_memory(provider, index),
                )
                .is_ok()
        );
    }

    /// ⚠️ **This test asserted the OPPOSITE and was wrong.** A memory *instance*'s type has
    /// `min = its current page count`, and `memory.grow` updates it (§4.5.9), so a memory declared
    /// `(memory 1)` and grown to 4 **does** satisfy an `(import … (memory 4))`.
    ///
    /// The original version stored a declared minimum and asserted that growth could not change what
    /// links. No memory case in the spec suite contradicted it, so it stood — until the equivalent
    /// **table** case did, in `table_grow.wast`, whose own comment says "imported table limits should
    /// match, because external table size is 2 now". Kept as the positive statement of the real rule.
    #[test]
    fn memory_import_matching_uses_the_current_size_which_growth_updates() {
        let mut store = Store::new();
        let mk = |src: &[u8]| decode(&crate::wat::assemble(src).unwrap()).unwrap();
        let provider = store
            .instantiate(
                mk(br#"(module (memory (export "m") 1)
                        (func (export "grow") (result i32) (memory.grow (i32.const 3))))"#),
                Imports::new(),
            )
            .unwrap();
        assert_eq!(as_i32(store.invoke(provider, "grow", &[]).unwrap()[0]), 1);
        let index = store
            .export_index(provider, "m", crate::types::ExternKind::Memory)
            .unwrap();
        // Declared 1, grown to 4 — so an import asking for 4 LINKS.
        assert!(
            store
                .instantiate(
                    mk(br#"(module (import "env" "m" (memory 4)))"#),
                    Imports::new().with_instance_memory(provider, index),
                )
                .is_ok(),
            "a grown memory satisfies an import up to its current size"
        );
        // And one asking for more than the current size still does not.
        assert_eq!(
            store
                .instantiate(
                    mk(br#"(module (import "env" "m" (memory 5)))"#),
                    Imports::new().with_instance_memory(provider, index),
                )
                .err(),
            Some(Trap::IncompatibleImport)
        );
    }

    // --- module linking (T7b) ---

    #[test]
    fn one_instance_calls_anothers_export() {
        // Provider exports `get` returning 7; consumer imports it and adds 35.
        let provider = crate::wat::assemble(
            br#"(module (func (export "get") (result i32) (i32.const 7)))"#,
        )
        .unwrap();
        let consumer = crate::wat::assemble(
            br#"(module
                  (import "p" "get" (func $get (result i32)))
                  (func (export "run") (result i32)
                    (i32.add (call $get) (i32.const 35))))"#,
        )
        .unwrap();

        let mut store = Store::new();
        let p = store
            .instantiate(decode(&provider).unwrap(), Imports::new())
            .unwrap();
        let get = store.export_func(p, "get").unwrap();
        let c = store
            .instantiate(
                decode(&consumer).unwrap(),
                Imports::new().with_instance_func(p, get),
            )
            .unwrap();
        assert_eq!(as_i32(store.invoke(c, "run", &[]).unwrap()[0]), 42);
    }

    #[test]
    fn a_linked_callee_sees_its_own_memory_not_the_callers() {
        // THE property that makes linking correct: the callee runs against ITS OWN
        // instance. Both modules have a memory; the provider's holds 11 at address 0 and
        // the consumer's holds 22. Calling the provider's reader must yield 11 — if the
        // callee ran against the caller's instance it would wrongly read 22.
        let provider = crate::wat::assemble(
            br#"(module
                  (memory 1)
                  (data (i32.const 0) "\0b\00\00\00")
                  (func (export "peek") (result i32) (i32.load (i32.const 0))))"#,
        )
        .unwrap();
        let consumer = crate::wat::assemble(
            br#"(module
                  (import "p" "peek" (func $peek (result i32)))
                  (memory 1)
                  (data (i32.const 0) "\16\00\00\00")
                  (func (export "run") (result i32)
                    (i32.mul (call $peek) (i32.const 100))))"#,
        )
        .unwrap();

        let mut store = Store::new();
        let p = store
            .instantiate(decode(&provider).unwrap(), Imports::new())
            .unwrap();
        let peek = store.export_func(p, "peek").unwrap();
        let c = store
            .instantiate(
                decode(&consumer).unwrap(),
                Imports::new().with_instance_func(p, peek),
            )
            .unwrap();
        // 11 * 100 — the provider's memory, not the consumer's 22.
        assert_eq!(as_i32(store.invoke(c, "run", &[]).unwrap()[0]), 1100);
    }

    #[test]
    fn linked_instances_keep_separate_globals() {
        // Each instance's mutable global is its own slot in the shared pools.
        let provider = crate::wat::assemble(
            br#"(module
                  (global $g (mut i32) (i32.const 5))
                  (func (export "bump") (result i32)
                    (global.set $g (i32.add (global.get $g) (i32.const 1)))
                    (global.get $g)))"#,
        )
        .unwrap();
        let consumer = crate::wat::assemble(
            br#"(module
                  (import "p" "bump" (func $bump (result i32)))
                  (global $g (mut i32) (i32.const 100))
                  (func (export "run") (result i32)
                    (global.set $g (i32.add (global.get $g) (i32.const 1)))
                    (i32.add (call $bump) (global.get $g))))"#,
        )
        .unwrap();

        let mut store = Store::new();
        let p = store
            .instantiate(decode(&provider).unwrap(), Imports::new())
            .unwrap();
        let bump = store.export_func(p, "bump").unwrap();
        let c = store
            .instantiate(
                decode(&consumer).unwrap(),
                Imports::new().with_instance_func(p, bump),
            )
            .unwrap();
        // provider 5->6, consumer 100->101 => 107. Sharing a global slot would give 2 or 202.
        assert_eq!(as_i32(store.invoke(c, "run", &[]).unwrap()[0]), 107);
    }

    #[test]
    fn a_trap_propagates_across_a_link() {
        let provider =
            crate::wat::assemble(br#"(module (func (export "boom") (result i32) (unreachable)))"#)
                .unwrap();
        let consumer = crate::wat::assemble(
            br#"(module
                  (import "p" "boom" (func $boom (result i32)))
                  (func (export "run") (result i32) (call $boom)))"#,
        )
        .unwrap();
        let mut store = Store::new();
        let p = store
            .instantiate(decode(&provider).unwrap(), Imports::new())
            .unwrap();
        let boom = store.export_func(p, "boom").unwrap();
        let c = store
            .instantiate(
                decode(&consumer).unwrap(),
                Imports::new().with_instance_func(p, boom),
            )
            .unwrap();
        assert_eq!(store.invoke(c, "run", &[]), Err(Trap::Unreachable));
    }

    #[test]
    fn mutual_recursion_across_instances_hits_the_depth_cap() {
        // A calls B calls A … must exhaust the call-stack guard rather than the host stack
        // or a borrow panic — the failure mode an Rc<RefCell> design would have had.
        let a_src = crate::wat::assemble(
            br#"(module
                  (import "b" "ping" (func $ping (result i32)))
                  (func (export "pong") (result i32) (call $ping)))"#,
        )
        .unwrap();
        let b_src = crate::wat::assemble(
            br#"(module (func (export "ping") (result i32) (i32.const 1)))"#,
        )
        .unwrap();
        let mut store = Store::new();
        let b = store
            .instantiate(decode(&b_src).unwrap(), Imports::new())
            .unwrap();
        let ping = store.export_func(b, "ping").unwrap();
        let a = store
            .instantiate(
                decode(&a_src).unwrap(),
                Imports::new().with_instance_func(b, ping),
            )
            .unwrap();
        assert_eq!(as_i32(store.invoke(a, "pong", &[]).unwrap()[0]), 1);
    }

    #[test]
    fn memory_size_reads_its_own_instance_not_the_pool_slot() {
        // The shared-store defect class, fourth instance (T9a#2): `Op::MemorySize` indexed
        // `store.memories` with the raw module-local immediate. Under ONE instance per
        // store the two indices are equal and the bug is invisible — so, per the standing
        // rule, this test instantiates a SECOND module. Instance `a` has 5 pages and takes
        // pool slot 0; `b` has 1 page and takes slot 1, but its `memory.size` immediate is
        // still 0. Unmapped, `b` answers 5.
        let a_src = crate::wat::assemble(br#"(module (memory 5))"#).unwrap();
        let b_src =
            crate::wat::assemble(br#"(module (memory 1) (func (export "sz") (result i32) memory.size))"#)
                .unwrap();
        let mut store = Store::new();
        let _a = store
            .instantiate(decode(&a_src).unwrap(), Imports::new())
            .unwrap();
        let b = store
            .instantiate(decode(&b_src).unwrap(), Imports::new())
            .unwrap();
        assert_eq!(as_i32(store.invoke(b, "sz", &[]).unwrap()[0]), 1);
    }

    // ---- Configurable resource limits (T8) ------------------------------------------
    //
    // Each of these was a compile-time constant before T8. The tests lower a ceiling and
    // show the *documented trap* appears — a limit that could not be observed to bite
    // would be configuration theatre.

    fn wat_module(src: &str) -> Module {
        decode(&crate::wat::assemble(src.as_bytes()).expect("assemble")).expect("decode")
    }

    #[test]
    fn lowering_the_call_depth_makes_recursion_trap_sooner() {
        // Self-recursive with no base case: traps either way. What the limit changes is
        // *when* — so the test pins that a shallow ceiling still yields the same trap
        // rather than exhausting the host stack.
        let md = wat_module(
            r#"(module (func $f (export "go") (result i32) (call $f)) )"#,
        );
        let mut inst = Instance::new_with(
            md,
            Imports::new(),
            ResourceLimits {
                max_call_depth: 8,
                ..ResourceLimits::defaults()
            },
        )
        .unwrap();
        assert_eq!(inst.invoke("go", &[]), Err(Trap::CallStackExhausted));
    }

    #[test]
    fn the_default_call_depth_still_matches_the_frozen_oracle() {
        // 512 is oracle parity and must not drift just because it became configurable.
        assert_eq!(ResourceLimits::defaults().max_call_depth, 512);
        assert_eq!(Store::new().limits(), ResourceLimits::defaults());
    }

    #[test]
    fn a_lowered_memory_ceiling_refuses_instantiation() {
        // 4 pages = 256 KiB declared, against a 128 KiB ceiling.
        let md = wat_module("(module (memory 4))");
        assert_eq!(
            Instance::new_with(
                md,
                Imports::new(),
                ResourceLimits {
                    max_memory_bytes: 2 * PAGE_SIZE,
                    ..ResourceLimits::defaults()
                }
            )
            .err(),
            Some(Trap::MemoryLimitExceeded)
        );
        // The same module is fine under the default ceiling.
        assert!(Instance::new(wat_module("(module (memory 4))")).is_ok());
    }

    #[test]
    fn a_lowered_memory_ceiling_also_refuses_growth() {
        // The ceiling has to hold at `memory.grow` too, not just at instantiation —
        // otherwise a guest declaring 1 page could grow straight past it.
        let md = wat_module(
            r#"(module (memory 1) (func (export "g") (result i32)
                 (memory.grow (i32.const 4))))"#,
        );
        let mut inst = Instance::new_with(
            md,
            Imports::new(),
            ResourceLimits {
                max_memory_bytes: 2 * PAGE_SIZE,
                ..ResourceLimits::defaults()
            },
        )
        .unwrap();
        // `memory.grow` reports refusal as -1; it does not trap.
        assert_eq!(as_i32(inst.invoke("g", &[]).unwrap()[0]), -1);
    }

    #[test]
    fn a_lowered_gc_object_ceiling_exhausts_the_heap() {
        let md = wat_module(
            r#"(module (type $s (struct (field i32)))
                 (func (export "alloc") (param i32)
                   (local $i i32)
                   (loop $l
                     (drop (struct.new $s (i32.const 1)))
                     (local.set $i (i32.add (local.get $i) (i32.const 1)))
                     (br_if $l (i32.lt_s (local.get $i) (local.get 0))))))"#,
        );
        let mut inst = Instance::new_with(
            md,
            Imports::new(),
            ResourceLimits {
                max_gc_objects: 4,
                ..ResourceLimits::defaults()
            },
        )
        .unwrap();
        assert_eq!(
            inst.invoke("alloc", &[i32_value(100)]),
            Err(Trap::GcHeapExhausted)
        );
    }

    #[test]
    fn a_lowered_table_ceiling_refuses_instantiation() {
        let md = wat_module("(module (table 100 funcref))");
        assert_eq!(
            Instance::new_with(
                md,
                Imports::new(),
                ResourceLimits {
                    max_table_elems: 10,
                    ..ResourceLimits::defaults()
                }
            )
            .err(),
            Some(Trap::TableLimitExceeded)
        );
    }

    #[test]
    fn raising_a_ceiling_lets_a_bigger_guest_run() {
        // The point of making these reachable: a guest that needs more than the shipped
        // default can now be run at all, instead of being refused by a constant.
        let md = wat_module("(module (table 100 funcref))");
        assert!(Instance::new_with(
            md,
            Imports::new(),
            ResourceLimits {
                max_table_elems: 1000,
                ..ResourceLimits::defaults()
            }
        )
        .is_ok());
    }

    fn write_uleb(out: &mut Vec<u8>, mut v: u32) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                out.push(b | 0x80);
            } else {
                out.push(b);
                break;
            }
        }
    }
}

#[cfg(test)]
mod backtrace_tests {
    use super::*;
    use crate::module::decode;

    fn store_with(src: &[u8]) -> (Store, InstanceId) {
        let bytes = crate::wat::assemble(src).unwrap();
        let md = decode(&bytes).unwrap();
        let mut store = Store::new();
        let id = store.instantiate(md, Imports::default()).unwrap();
        (store, id)
    }

    fn func_index(store: &Store, id: InstanceId, name: &str) -> u32 {
        store.export_func(id, name).unwrap()
    }

    /// The whole point of the feature: a trap three calls deep names all three, innermost first.
    /// A single-frame test would pass even if the recursion never recorded anything.
    #[test]
    fn a_trap_reports_every_frame_innermost_first() {
        let (mut store, id) = store_with(
            br#"(module
                  (func $bottom (unreachable))
                  (func $middle (call $bottom))
                  (func $top (export "top") (call $middle)))"#,
        );
        let top = func_index(&store, id, "top");
        assert_eq!(store.invoke_index(id, top, &[]), Err(Trap::Unreachable));

        let frames: Vec<u32> = store.backtrace().iter().map(|f| f.func_index).collect();
        assert_eq!(frames, vec![0, 1, 2], "innermost ($bottom = 0) must come first");
        assert!(store.backtrace().iter().all(|f| f.instance == 0));
    }

    /// The offsets must be *distinct and increasing within the module*, and must actually point at
    /// the trapping instruction. A constant or a zero would satisfy a count-only assertion.
    #[test]
    fn offsets_point_at_the_trapping_instruction() {
        let (mut store, id) = store_with(
            br#"(module
                  (func $bottom (unreachable))
                  (func $top (export "top") (call $bottom)))"#,
        );
        let top = func_index(&store, id, "top");
        let _ = store.invoke_index(id, top, &[]);
        let bt = store.backtrace().to_vec();
        assert_eq!(bt.len(), 2);

        // Frame 0 is `unreachable`, the first instruction of $bottom's body; frame 1 is the `call`,
        // the first instruction of $top's body. Both therefore sit exactly at their body's start,
        // and $top's body comes later in the module than $bottom's.
        let slot = store.slot(id).unwrap();
        let code = &store.code[slot].module.code;
        assert_eq!(bt[0].offset, code[0].body_offset);
        assert_eq!(bt[1].offset, code[1].body_offset);
        assert!(bt[1].offset > bt[0].offset);
    }

    /// A non-first instruction: the offset must advance past the ones before it, which is what
    /// distinguishes a real pc from "the body start".
    #[test]
    fn the_offset_advances_within_a_body() {
        let (mut store, id) = store_with(
            br#"(module
                  (func (export "t") (result i32)
                    (i32.const 1) (drop)
                    (i32.const 1) (i32.const 0) (i32.div_s)))"#,
        );
        let t = func_index(&store, id, "t");
        assert_eq!(store.invoke_index(id, t, &[]), Err(Trap::DivByZero));
        let bt = store.backtrace();
        let slot = store.slot(id).unwrap();
        let base = store.code[slot].module.code[0].body_offset;
        // i32.const 1 (2) + drop (1) + i32.const 1 (2) + i32.const 0 (2) = 7 bytes before div_s.
        assert_eq!(bt[0].offset - base, 7);
    }

    /// A successful call must leave no backtrace behind. Without the per-invocation clear, an
    /// embedder that checks the backtrace unconditionally would report the *previous* failure.
    #[test]
    fn success_clears_a_previous_backtrace() {
        let (mut store, id) = store_with(
            br#"(module
                  (func (export "bad") (unreachable))
                  (func (export "good") (result i32) (i32.const 7)))"#,
        );
        let bad = func_index(&store, id, "bad");
        let good = func_index(&store, id, "good");
        let _ = store.invoke_index(id, bad, &[]);
        assert!(!store.backtrace().is_empty());
        assert_eq!(store.invoke_index(id, good, &[]), Ok(vec![7]));
        assert!(store.backtrace().is_empty(), "a success must not report the last trap");
    }

    /// A caught exception is not a trap. The frames its unwind passed through describe a failure
    /// that did not happen, so a later real trap must not inherit them.
    #[test]
    fn a_caught_exception_does_not_leave_frames() {
        let (mut store, id) = store_with(
            br#"(module
                  (tag $e)
                  (func $thrower (throw $e))
                  (func (export "t") (result i32)
                    (block $h
                      (try_table (catch $e $h) (call $thrower))
                      (return (i32.const 1)))
                    (unreachable)))"#,
        );
        let t = func_index(&store, id, "t");
        assert_eq!(store.invoke_index(id, t, &[]), Err(Trap::Unreachable));
        // Only the `unreachable` after the handler — NOT the two frames the throw unwound through.
        let frames: Vec<u32> = store.backtrace().iter().map(|f| f.func_index).collect();
        assert_eq!(frames, vec![1], "the caught throw's frames must have been discarded");
    }

    /// Names come from the name section when there is one.
    #[test]
    fn frames_resolve_to_names_when_the_module_has_them() {
        let (mut store, id) = store_with(
            br#"(module (func $boom (export "boom") (unreachable)))"#,
        );
        let boom = func_index(&store, id, "boom");
        let _ = store.invoke_index(id, boom, &[]);
        let frame = store.backtrace()[0];
        // The assembler emits no name section, so this must report None rather than guess.
        assert_eq!(store.frame_name(&frame), None);
    }
}

#[cfg(test)]
mod start_tests {
    use super::*;
    use crate::module::decode;

    fn build(src: &[u8]) -> Module {
        decode(&crate::wat::assemble(src).unwrap()).unwrap()
    }

    /// §4.5.5 step 11. The start function was decoded, validated and printed by the CLI but never
    /// *run* — a silent wrong answer rather than a failure, which is why 10 suite assertions sat
    /// unexplained instead of pointing at it.
    #[test]
    fn the_start_function_runs_at_instantiation() {
        let mut store = Store::new();
        let id = store
            .instantiate(
                build(
                    br#"(module
                          (global $g (mut i32) (i32.const 0))
                          (func $init (global.set $g (i32.const 42)))
                          (start $init)
                          (func (export "get") (result i32) (global.get $g)))"#,
                ),
                Imports::default(),
            )
            .unwrap();
        assert_eq!(store.invoke(id, "get", &[]), Ok(vec![42]));
    }

    /// It runs LAST — after data and element segments — so it can observe them. Running it earlier
    /// would still make the test above pass.
    #[test]
    fn the_start_function_sees_the_data_segments() {
        let mut store = Store::new();
        let id = store
            .instantiate(
                build(
                    br#"(module
                          (memory 1)
                          (data (i32.const 0) "\07")
                          (global $g (mut i32) (i32.const 0))
                          (func $init (global.set $g (i32.load8_u (i32.const 0))))
                          (start $init)
                          (func (export "get") (result i32) (global.get $g)))"#,
                ),
                Imports::default(),
            )
            .unwrap();
        assert_eq!(store.invoke(id, "get", &[]), Ok(vec![7]));
    }

    /// A trap in the start function fails the instantiation; the caller must never receive an id
    /// for a module whose initialization did not complete.
    #[test]
    fn a_trapping_start_function_fails_instantiation() {
        let mut store = Store::new();
        let r = store.instantiate(
            build(br#"(module (func $boom (unreachable)) (start $boom))"#),
            Imports::default(),
        );
        assert_eq!(r, Err(Trap::Unreachable));
    }

    /// And it reports where it trapped, like any other call.
    #[test]
    fn a_trapping_start_function_still_produces_a_backtrace() {
        let mut store = Store::new();
        let _ = store.instantiate(
            build(br#"(module (func $boom (unreachable)) (start $boom))"#),
            Imports::default(),
        );
        assert_eq!(store.backtrace().len(), 1);
        assert_eq!(store.backtrace()[0].func_index, 0);
    }

    /// Only once. A second instantiation of the same module is a second instance with its own
    /// start run, but instantiating once must not run it twice.
    #[test]
    fn the_start_function_runs_exactly_once_per_instance() {
        let mut store = Store::new();
        let src = br#"(module
                        (global $g (mut i32) (i32.const 0))
                        (func $init (global.set $g (i32.add (global.get $g) (i32.const 1))))
                        (start $init)
                        (func (export "get") (result i32) (global.get $g)))"#;
        let a = store.instantiate(build(src), Imports::default()).unwrap();
        let b = store.instantiate(build(src), Imports::default()).unwrap();
        assert_eq!(store.invoke(a, "get", &[]), Ok(vec![1]));
        assert_eq!(store.invoke(b, "get", &[]), Ok(vec![1]), "each instance has its own global");
    }
}
