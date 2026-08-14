//! Type validation of a decoded module (WebAssembly spec §3, Appendix "Validation
//! Algorithm": an abstract operand-value stack + a control-frame stack, with a bottom
//! `unknown` type for stack-polymorphic / unreachable code).
//!
//! Ported from wazmrt `src/validate.zig` (T4, completed in v0.7.0). **Coverage is now the
//! whole language wasmrt executes:** control flow, calls, parametric/variable/reference ops,
//! tables, bulk memory, loads/stores/numeric, **SIMD** (the `0xFD` family), **threads/atomics**
//! (`0xFE`), **WasmGC** (struct/array objects, `i31`, casts, cast-branches), and **exception
//! handling** (both the `try_table` and legacy encodings) — plus module-level checks
//! (const-exprs, elements, data, limits, tags, exports, start) and the `C.refs`
//! (undeclared-function-reference) rule.
//!
//! Two deliberate refusals remain, both matching the frozen oracle: `delegate` (its label
//! routing is unimplementable against the interpreter — see the arm) and any atomic
//! sub-opcode outside the defined set. Both reject loudly
//! ([`ValidateError::UnsupportedValidation`]) — never silent-accept — so "the validator
//! accepted it" stays a trustworthy promise.
//!
//! **Proposal gating (T8):** [`validate_with_features`] additionally refuses any construct
//! whose proposal the engine was configured to reject ([`crate::features::Features`]).
//! Gating happens *here*, at validation, never at execution, so nothing part-way checked
//! reaches the interpreter. Plain [`validate`] enables everything and is unaffected.
//!
//! `validate` does not mutate the module; it decodes each body to IR and type-checks it.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::features::{
    op_feature, simd_sub_feature, table_element_feature, val_type_feature, Feature, Features,
};
use crate::module::{Code, CompType, FuncType, Module};
use crate::opcode::{self, HeapType, Imm, Instr, Op, RefType};
use crate::reader::Reader;
use crate::types::{DecodeError, RefHeap, ValType};

type V = ValType;

/// Cap on control nesting. Every `push_ctrl` snapshots the whole local-init vector, so the
/// cost is depth × locals — a memory amplifier a tiny module could otherwise drive. 1024
/// is far above real code and matches the text parser's depth cap.
const MAX_CTRL_DEPTH: usize = 1024;

/// Cap on a function's locals (params + declared). The run-length local encoding lets a few
/// bytes ask for billions; read together with [`MAX_CTRL_DEPTH`] (the snapshot cost is their
/// product). 50 000 matches wasmtime's default.
pub const MAX_LOCALS: u64 = 50_000;

/// Errors from validation. `Decode` wraps a decode error (bodies are decoded to IR here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateError {
    Decode(DecodeError),
    CountMismatch,
    TypeMismatch,
    StackUnderflow,
    StackHeightMismatch,
    ControlUnderflow,
    UnknownLabel,
    MismatchedElse,
    UndefinedLocal,
    UninitializedLocal,
    NestingTooDeep,
    TooManyLocals,
    UndefinedGlobal,
    ImmutableGlobal,
    UndefinedFunc,
    UndeclaredFuncRef,
    UndefinedType,
    UndefinedTag,
    InvalidTag,
    /// A legacy `catch`/`catch_all` whose enclosing opener is not a `try` (EH).
    MismatchedCatch,
    /// A GC struct/array field index out of range.
    UndefinedField,
    /// A `struct.set` / `array.set` on an immutable field.
    ImmutableField,
    InvalidLimits,
    DuplicateExport,
    UndefinedTable,
    UndefinedElem,
    UndefinedData,
    InvalidStartFunction,
    ConstantExpressionRequired,
    InvalidAlignment,
    MissingMemory,
    InvalidMemArgOffset,
    /// A construct the validator refuses to type-check. Loud by design — never a silent
    /// accept, so "the validator accepted it" stays a trustworthy promise.
    ///
    /// It no longer means "not yet ported": the SIMD / atomics / GC / EH arms it originally
    /// covered all landed in v0.7.0. Today it has exactly two uses:
    ///
    /// 1. **A deliberate refusal** — `delegate`, whose label routing the interpreter cannot
    ///    execute correctly and which the frozen oracle also rejects.
    /// 2. **An immediate-shape guard** — the decoder produced an immediate this opcode can
    ///    never carry (`let Imm::BrTable(..) = … else`). Unreachable through
    ///    [`decode_body`], and refused rather than assumed away.
    UnsupportedValidation,
    /// A type declares a supertype it does not actually subtype (§3.4.5): a different composite
    /// kind, an incompatible field or signature, or a supertype that is **final**.
    ///
    /// Its own variant rather than `TypeMismatch` because the two are found at different stages and
    /// mean different things to whoever reads the error — this one is a malformed type *hierarchy*,
    /// not an ill-typed instruction.
    SubType,
    /// The module uses a proposal this engine was configured to reject
    /// ([`crate::features::Features`]). Carries the proposal so an embedder can say which.
    FeatureDisabled(Feature),
}

impl From<DecodeError> for ValidateError {
    fn from(e: DecodeError) -> Self {
        ValidateError::Decode(e)
    }
}

impl fmt::Display for ValidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidateError::Decode(e) => write!(f, "decode error during validation: {e}"),
            ValidateError::FeatureDisabled(x) => {
                write!(f, "invalid module: uses the disabled proposal `{x}`")
            }
            other => write!(f, "invalid module: {other:?}"),
        }
    }
}

impl core::error::Error for ValidateError {}

/// A validation result.
pub type ValidateResult<T> = core::result::Result<T, ValidateError>;

/// Validate an entire module against **every** proposal wasmrt implements.
///
/// Equivalent to [`validate_with_features`] with [`Features::all`] — the pre-T8 behaviour,
/// and what the CLI, the `.wast` runner and the assembler's tests use.
pub fn validate(module: &Module) -> ValidateResult<()> {
    validate_with_features(module, &Features::all())
}

/// Validate an entire module, rejecting any proposal disabled in `features`. Returns on the
/// first error.
///
/// Gating happens **here**, at validation, and never at execution: a module that names a
/// disabled proposal is simply invalid, so nothing half-checked reaches the interpreter.
/// With [`Features::all`] every gate below is a no-op, which is why enabling this cost the
/// spec-suite numbers nothing.
pub fn validate_with_features(module: &Module, features: &Features) -> ValidateResult<()> {
    // Cleared on ENTRY, not on success: every failure below this point that is not inside a
    // function body must report "no location" rather than inherit the previous module's.
    #[cfg(feature = "std")]
    SITE.with(|c| c.set(FailureSite::default()));

    if module.functions.len() != module.code.len() {
        return Err(ValidateError::CountMismatch);
    }

    check_module_features(module, features)?;
    check_declared_subtyping(module)?;

    // C.refs (§3.4.10, "undeclared function reference"): a `ref.func x` inside a function
    // body is well-typed only if `x` also occurs outside the code section. Populate from
    // the module-level structures, then consult it in the body validator.
    let n_funcs = module.imported_func_count() as usize + module.functions.len();
    let mut refs = vec![false; n_funcs];
    for e in &module.exports {
        if e.ty.kind() == crate::types::ExternKind::Func && (e.index as usize) < n_funcs {
            refs[e.index as usize] = true;
        }
    }
    if let Some(si) = module.start {
        if (si as usize) < n_funcs {
            refs[si as usize] = true;
        }
    }

    // Global init const-exprs: each must produce exactly the declared type. Defined globals
    // occupy the tail of the global space.
    let n_imported_globals = (module.globals.len() - module.global_inits.len()) as u32;
    for (i, init_expr) in module.global_inits.iter().enumerate() {
        let self_index = n_imported_globals + i as u32;
        let expected = module.globals[self_index as usize].content;
        validate_const_expr(module, init_expr, expected, self_index, Some(&mut refs), features)?;
    }

    let all_globals = module.globals.len() as u32;

    // Element segments.
    for elem in &module.elements {
        for &fi in &elem.funcs {
            if module.func_type(fi).is_none() {
                return Err(ValidateError::UndefinedFunc);
            }
            if (fi as usize) < n_funcs {
                refs[fi as usize] = true; // a segment entry declares the function
            }
        }
        for ex in &elem.exprs {
            validate_const_expr(
                module,
                ex,
                elem.elem_type,
                n_imported_globals,
                Some(&mut refs),
                features,
            )?;
        }
        if elem.mode == crate::module::ElementMode::Active {
            let ti = elem.table_index as usize;
            if ti >= module.tables.len() {
                return Err(ValidateError::UndefinedTable);
            }
            let tet = module.tables[ti].element;
            // §3.5.9: the segment's element type must be a SUBTYPE of the table's, which
            // is directional and does care about nullability.
            //
            // This used to compare families with nullability normalized away. That was
            // harmless only because a non-nullable table element type could not occur —
            // the decoder rejected the `0x40` table-with-initializer form outright. Once
            // that form decodes (2026-08-06), `(table 1 (ref func))` is expressible and a
            // nullable `funcref` segment must no longer satisfy it.
            if !subtype_of(module, elem.elem_type, tet) {
                return Err(ValidateError::TypeMismatch);
            }
            validate_const_expr(module, &elem.offset_expr, V::I32, all_globals, None, features)?;
        }
    }

    // Data segments: an active segment targets an existing memory; its offset const-expr
    // produces the memory's index type (memory64: i64, else i32).
    for seg in &module.data {
        if !seg.active {
            continue;
        }
        let mi = seg.mem_index as usize;
        if mi >= module.memories.len() {
            return Err(ValidateError::MissingMemory);
        }
        let off_ty = if module.memories[mi].limits.is64 {
            V::I64
        } else {
            V::I32
        };
        validate_const_expr(module, &seg.offset_expr, off_ty, all_globals, None, features)?;
    }

    // Limits (§3.2.5): min <= max, each bounded by the type ceiling; a shared memory must
    // declare a max.
    for mt in &module.memories {
        let ceiling: u64 = if mt.limits.is64 {
            0x1_0000_0000_0000
        } else {
            0x1_0000
        };
        if mt.limits.min > ceiling {
            return Err(ValidateError::InvalidLimits);
        }
        match mt.limits.max {
            Some(mx) => {
                if mx > ceiling || mt.limits.min > mx {
                    return Err(ValidateError::InvalidLimits);
                }
            }
            None => {
                if mt.limits.shared {
                    return Err(ValidateError::InvalidLimits);
                }
            }
        }
    }
    for tt in &module.tables {
        if let Some(mx) = tt.limits.max {
            if tt.limits.min > mx {
                return Err(ValidateError::InvalidLimits);
            }
        }
        // A table's initializer expression must produce its element type
        // (function-references). Checked against ALL globals, since a table is defined
        // after every global in the index space.
        if let Some(expr) = &tt.init {
            validate_const_expr(module, expr, tt.element, all_globals, Some(&mut refs), features)?;
        }
    }

    // Tag types (§3.2, EH): a tag's type must be `[t1*] → []`.
    for &ti in &module.tags {
        let ft = module.func_sig(ti).ok_or(ValidateError::UndefinedType)?;
        if !ft.results.is_empty() {
            return Err(ValidateError::InvalidTag);
        }
    }

    // Export names must be pairwise distinct (§3.4.10).
    for (i, e) in module.exports.iter().enumerate() {
        for o in &module.exports[i + 1..] {
            if e.name == o.name {
                return Err(ValidateError::DuplicateExport);
            }
        }
    }

    // Start function (§3.5.5): a defined/imported function of type [] → [].
    if let Some(si) = module.start {
        let ft = module.func_type(si).ok_or(ValidateError::UndefinedFunc)?;
        if !ft.params.is_empty() || !ft.results.is_empty() {
            return Err(ValidateError::InvalidStartFunction);
        }
    }

    // Function bodies last, so `refs` is complete before any body's C.refs check.
    for (n, (&type_index, code)) in module.functions.iter().zip(&module.code).enumerate() {
        let ft = module.func_sig(type_index).ok_or(ValidateError::UndefinedType)?;
        validate_function(module, &ft, code, Some(&refs), features).map_err(|e| {
            // Attach WHICH function failed. A bare `TypeMismatch` for a 900-line module is a
            // diagnosis problem, not a verdict problem: localizing T9a#9's fixture by hand — to
            // defined function #6 of 19 — is what turned "our type-checker is wrong" into "the
            // module is ill-typed and both validators agree".
            located(e, module.imported_func_count() + n as u32)
        })?;
    }
    Ok(())
}

/// Where the last validation failure was, and what it was about.
///
/// **Shaped to match wasmtime**, which is the standard this project holds itself to on diagnostics
/// as well as behaviour. For the module `(func (result i32) i64.const 1)` wasmtime 47 says:
///
/// ```text
/// Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64
/// ```
///
/// So a useful report needs three things a bare error variant cannot carry: **where** in the module,
/// **what was expected**, and **what was found**.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FailureSite {
    /// Index in the function index space (imports included), if the failure was inside a body.
    pub func_index: Option<u32>,
    /// Byte offset **from the start of the module** of the instruction that failed — the same
    /// number, and the same origin, that wasmtime prints.
    pub offset: Option<u32>,
    /// For a type mismatch: what the instruction required, and what was actually on the stack.
    pub expected: Option<V>,
    pub found: Option<V>,
}

// A thread-local rather than a change to `ValidateError`: the error type is `Copy`, is matched
// exhaustively in several places, and crosses the C ABI, so widening it to carry a payload would be
// a breaking change for a diagnostic. `validate` is not re-entrant across threads in any caller,
// and a stale value can only make a *later* failure's location wrong, never a success look failed.
#[cfg(feature = "std")]
std::thread_local! {
    static SITE: core::cell::Cell<FailureSite> = const { core::cell::Cell::new(FailureSite {
        func_index: None, offset: None, expected: None, found: None,
    }) };
}

/// Record which function a failure came from. Called as the error leaves the per-body loop.
#[cfg(feature = "std")]
fn located(e: ValidateError, func_index: u32) -> ValidateError {
    SITE.with(|c| {
        let mut s = c.get();
        s.func_index = Some(func_index);
        c.set(s);
    });
    e
}

#[cfg(not(feature = "std"))]
fn located(e: ValidateError, _func_index: u32) -> ValidateError {
    e
}

/// Record the absolute module offset of the instruction that failed.
///
/// Set at the **innermost** point that knows it and never overwritten on the way out, so a nested
/// helper's position wins over the enclosing instruction's — which is what makes the offset point at
/// the operand that was actually wrong.
#[cfg(feature = "std")]
fn note_offset(offset: u32) {
    SITE.with(|c| {
        let mut s = c.get();
        if s.offset.is_none() {
            s.offset = Some(offset);
        }
        c.set(s);
    });
}

#[cfg(not(feature = "std"))]
fn note_offset(_offset: u32) {}

/// Record the two types of a mismatch, at the one place that knows both.
#[cfg(feature = "std")]
fn note_types(expected: V, found: V) {
    SITE.with(|c| {
        let mut s = c.get();
        s.expected = Some(expected);
        s.found = Some(found);
        c.set(s);
    });
}

#[cfg(not(feature = "std"))]
fn note_types(_expected: V, _found: V) {}

/// Everything known about the most recent [`validate`] failure — see [`FailureSite`].
///
/// Valid only until the next `validate` call on this thread, and all-`None` after a success.
/// `no_std` builds always report `None`s: the record costs a thread-local, and a freestanding
/// embedder has nowhere to print it.
#[must_use]
pub fn last_failure_site() -> FailureSite {
    #[cfg(feature = "std")]
    {
        SITE.with(core::cell::Cell::get)
    }
    #[cfg(not(feature = "std"))]
    {
        FailureSite::default()
    }
}

/// The function index the most recent [`validate`] failure occurred in — [`last_failure_site`]'s
/// `func_index`, kept as its own function because that is the common case.
#[must_use]
pub fn last_failure_func_index() -> Option<u32> {
    last_failure_site().func_index
}

/// Reject a value type whose proposal is disabled.
fn gate_val_type(v: V, features: &Features) -> ValidateResult<()> {
    gate(val_type_feature(v), features)
}

/// Reject `f` if it is `Some` and disabled. The single spelling of the gate, so every call
/// site produces the same error.
fn gate(f: Option<Feature>, features: &Features) -> ValidateResult<()> {
    match f {
        Some(x) if !features.has(x) => Err(ValidateError::FeatureDisabled(x)),
        _ => Ok(()),
    }
}

/// Every proposal a module names through its **declarations** — types, limits, segment
/// modes — as opposed to its instructions (gated in [`FuncValidator::step`]).
///
/// Declarations are checked separately from instructions on purpose: a disabled proposal
/// must not be reachable through a *type* while all its opcodes are refused. A module
/// could otherwise declare a `v128` global, or an `(array …)` type, with SIMD or GC off.
/// Every declared supertype must actually be one (§3.4.5) — and must be open to being one.
///
/// wasmrt had **no** check here: `module.supertypes` was populated at decode and then only ever
/// walked by `Module::is_subtype`, which trusts it. So a module could declare any type as the
/// supertype of any other and the whole reference-subtyping story would rest on a lie —
/// `type-subtyping.wast` measured **21 invalid modules accepted** on exactly this. Two independent
/// rules, and a module can break either:
///
/// 1. **Finality.** A type is final unless declared `(sub …)` (`0x50`); `0x4f` is `sub final` and a
///    bare composite type is shorthand for `sub final ϵ`. A final type cannot be extended.
/// 2. **Structural matching.** The subtype's composite type must match the supertype's: same kind,
///    functions contravariant in parameters and covariant in results, structs extending by appending
///    fields, and each shared field matching per [`field_matches`].
fn check_declared_subtyping(module: &Module) -> ValidateResult<()> {
    for (i, sup) in module.supertypes.iter().enumerate() {
        let Some(s) = *sup else { continue };
        let s = s as usize;
        // The decoder already refuses a forward or self supertype, so `s < i` holds and this cannot
        // walk in a circle — but read it out of the module rather than assuming, because
        // `Module`'s fields are public and `validate` may be handed one nobody decoded.
        let (Some(sub_ct), Some(sup_ct)) = (module.comp_types.get(i), module.comp_types.get(s))
        else {
            return Err(ValidateError::UndefinedType);
        };
        // `type_finals` defaults to final for anything not recorded: a missing entry means "we do
        // not know it is open", and the safe reading of that is to refuse the extension.
        if module.type_finals.get(s).copied().unwrap_or(true) {
            return Err(ValidateError::SubType);
        }
        if !comp_type_matches(module, sub_ct, sup_ct) {
            return Err(ValidateError::SubType);
        }
    }
    Ok(())
}

/// Does composite type `sub` match `sup` (§3.4.5)? The variance is the whole content of this
/// function, and getting a direction backwards is silent: it would accept a hierarchy that lets a
/// caller pass the wrong type to a `call_ref`.
fn comp_type_matches(module: &Module, sub: &CompType, sup: &CompType) -> bool {
    match (sub, sup) {
        (CompType::Func(a), CompType::Func(b)) => {
            a.params.len() == b.params.len()
                && a.results.len() == b.results.len()
                // Parameters are CONTRAVARIANT: the subtype must accept everything the supertype
                // accepts, so the supertype's parameter must be a subtype of the subtype's.
                && b.params
                    .iter()
                    .zip(&a.params)
                    .all(|(&bp, &ap)| subtype_of(module, bp, ap))
                // Results are COVARIANT: the subtype may promise something more specific.
                && a.results
                    .iter()
                    .zip(&b.results)
                    .all(|(&ar, &br)| subtype_of(module, ar, br))
        }
        // A struct subtype may APPEND fields, never remove or reorder them: the prefix must match
        // so that any code holding the supertype reads the same fields at the same indices.
        (CompType::Struct(a), CompType::Struct(b)) => {
            a.len() >= b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(af, bf)| field_matches(module, af, bf))
        }
        (CompType::Array(a), CompType::Array(b)) => field_matches(module, a, b),
        // Different kinds never match — `(sub $an_array (struct))` is the case the suite tests.
        _ => false,
    }
}

/// Does field type `sub` match `sup`? Mutability must be **equal**, and it decides the variance:
/// an immutable field is covariant (read-only, so narrowing is safe), a mutable one is invariant
/// (it is also written through, so narrowing would let a write of the wider type land in it).
fn field_matches(
    module: &Module,
    sub: &crate::module::FieldType,
    sup: &crate::module::FieldType,
) -> bool {
    use crate::module::StorageType;
    if sub.mutable != sup.mutable {
        return false;
    }
    match (sub.storage, sup.storage) {
        // A packed field matches only the identical packing: `i8` and `i16` are distinct storage,
        // and neither is a value type.
        (StorageType::I8, StorageType::I8) | (StorageType::I16, StorageType::I16) => true,
        (StorageType::Val(a), StorageType::Val(b)) => {
            if sup.mutable {
                // A mutable field is INVARIANT — written through as well as read — so the two must
                // be the same type. `==` is the same index-vs-structure approximation as
                // `decl_subtype_of` and errs the same way, accepting what it cannot separate.
                // Invariant: it must be the *same* type, decided canonically rather than by index —
                // two spellings of one type are one type, and `==` on the packed bits misses that.
                // Nullability is still part of the type, so it must match too: `(mut (ref $t))` and
                // `(mut (ref null $t))` are different fields.
                a == b
                    || (a.is_concrete()
                        && b.is_concrete()
                        && a.is_non_null_ref() == b.is_non_null_ref()
                        && module.types_equal(a.concrete_index(), b.concrete_index()))
            } else {
                subtype_of(module, a, b)
            }
        }
        _ => false,
    }
}

fn check_module_features(module: &Module, features: &Features) -> ValidateResult<()> {
    if *features == Features::all() {
        return Ok(()); // the default: nothing to gate, and no walk to pay for
    }

    // --- the type index space ---
    for (i, ct) in module.comp_types.iter().enumerate() {
        // A declared supertype is GC's `(sub …)` form.
        if module.supertypes.get(i).copied().flatten().is_some() {
            gate(Some(Feature::Gc), features)?;
        }
        match ct {
            CompType::Func(ft) => {
                if ft.results.len() > 1 {
                    gate(Some(Feature::MultiValue), features)?;
                }
                for &t in ft.params.iter().chain(&ft.results) {
                    gate_val_type(t, features)?;
                }
            }
            CompType::Struct(fields) => {
                gate(Some(Feature::Gc), features)?;
                for f in fields {
                    gate_val_type(f.storage.unpacked(), features)?;
                }
            }
            CompType::Array(f) => {
                gate(Some(Feature::Gc), features)?;
                gate_val_type(f.storage.unpacked(), features)?;
            }
        }
    }

    // --- memories: count, index width, sharing ---
    if module.memories.len() > 1 {
        gate(Some(Feature::MultiMemory), features)?;
    }
    for mt in &module.memories {
        if mt.limits.is64 {
            gate(Some(Feature::Memory64), features)?;
        }
        if mt.limits.shared {
            gate(Some(Feature::Threads), features)?;
        }
    }

    // --- tables: a second table, or any element type other than `funcref` ---
    if module.tables.len() > 1 {
        gate(Some(Feature::ReferenceTypes), features)?;
    }
    for tt in &module.tables {
        gate(table_element_feature(tt.element), features)?;
    }

    // --- globals ---
    for g in &module.globals {
        gate_val_type(g.content, features)?;
    }

    // --- tags (EH). Imported tags are counted through `imports` below. ---
    if !module.tags.is_empty() {
        gate(Some(Feature::Exceptions), features)?;
    }
    for imp in &module.imports {
        if matches!(imp.ty, crate::module::Extern::Tag(_)) {
            gate(Some(Feature::Exceptions), features)?;
        }
    }

    // --- segments: a passive or declarative segment exists only to be named by a
    // bulk-memory instruction, so the mode itself is the proposal. ---
    for seg in &module.data {
        if !seg.active {
            gate(Some(Feature::BulkMemory), features)?;
        }
    }
    for elem in &module.elements {
        if elem.mode != crate::module::ElementMode::Active {
            gate(Some(Feature::BulkMemory), features)?;
        }
        // The const-expr element form (`(elem (ref func) (item …))`) is reference-types;
        // so is any element type other than plain `funcref`.
        if !elem.exprs.is_empty() {
            gate(Some(Feature::ReferenceTypes), features)?;
        }
        gate(table_element_feature(elem.elem_type), features)?;
    }

    Ok(())
}

/// Type-check a constant expression (§3.3.7 + extended-const `i32`/`i64` add/sub/mul). It
/// must produce exactly one value of `expected`. A `global.get x` may reference only a
/// *prior* immutable global. (v128.const and GC constant instructions land in v0.5.x.)
fn validate_const_expr(
    module: &Module,
    expr: &[u8],
    expected: V,
    self_index: u32,
    mut refs: Option<&mut Vec<bool>>,
    features: &Features,
) -> ValidateResult<()> {
    let mut r = Reader::new(expr);
    let mut stack: Vec<V> = Vec::new();
    let push = |stack: &mut Vec<V>, t: V| -> ValidateResult<()> {
        if stack.len() >= 8 {
            return Err(ValidateError::ConstantExpressionRequired);
        }
        stack.push(t);
        Ok(())
    };
    loop {
        match r.read_byte()? {
            0x0b => break, // end
            0x41 => {
                r.read_var_i32()?;
                push(&mut stack, V::I32)?;
            }
            0x42 => {
                r.read_var_i64()?;
                push(&mut stack, V::I64)?;
            }
            0x43 => {
                r.read_bytes(4)?;
                push(&mut stack, V::F32)?;
            }
            0x44 => {
                r.read_bytes(8)?;
                push(&mut stack, V::F64)?;
            }
            0x23 => {
                // global.get x — only a prior, immutable global.
                let gi = r.read_var_u32()?;
                if gi >= self_index {
                    return Err(ValidateError::UndefinedGlobal);
                }
                if module.globals[gi as usize].mutable {
                    return Err(ValidateError::ConstantExpressionRequired);
                }
                push(&mut stack, module.globals[gi as usize].content)?;
            }
            0xd0 => {
                // ref.null <heaptype>
                gate(Some(Feature::ReferenceTypes), features)?;
                let heap =
                    opcode::read_heap_type(&mut r).map_err(|_| ValidateError::ConstantExpressionRequired)?;
                let vt = ref_type_val_type(
                    module,
                    RefType {
                        nullable: true,
                        heap,
                    },
                )?;
                gate_val_type(vt, features)?;
                push(&mut stack, vt)?;
            }
            0xd2 => {
                // ref.func x
                gate(Some(Feature::ReferenceTypes), features)?;
                let fi = r.read_var_u32()?;
                if module.func_type(fi).is_none() {
                    return Err(ValidateError::UndefinedFunc);
                }
                if let Some(set) = refs.as_deref_mut() {
                    if (fi as usize) < set.len() {
                        set[fi as usize] = true; // ref.func outside a body DECLARES it (C.refs)
                    }
                }
                if let Some(ti) = module.func_type_index(fi) {
                    push(&mut stack, V::concrete_ref(false, RefHeap::Func, ti))?;
                } else {
                    push(&mut stack, V::FUNCREF_NN)?;
                }
            }
            // The GC constant forms (§3.3.11). Typed here exactly as their instruction
            // counterparts are in the body validator — the *same* six the interpreter evaluates, so
            // the two cannot disagree about what a constant expression is. They were rejected by
            // both, which was consistent but stopped whole modules building: a rejected global
            // initializer takes every later assertion in the file with it.
            0xfb => {
                gate(Some(Feature::Gc), features)?;
                let sub = r.read_var_u32()?;
                match sub {
                    // struct.new t / struct.new_default t
                    0x00 | 0x01 => {
                        let ti = r.read_var_u32()?;
                        let fields = module
                            .struct_fields(ti)
                            .ok_or(ValidateError::UndefinedType)?
                            .to_vec();
                        if sub == 0x00 {
                            // Fields are popped in declaration order, so check them in reverse.
                            for f in fields.iter().rev() {
                                let got = stack.pop().ok_or(ValidateError::StackUnderflow)?;
                                if !subtype_of(module, got, f.storage.unpacked()) {
                                    return Err(ValidateError::TypeMismatch);
                                }
                            }
                        }
                        push(&mut stack, V::concrete_ref(false, RefHeap::Struct, ti))?;
                    }
                    // array.new t (init, len) / array.new_default t (len)
                    0x06 | 0x07 => {
                        let ti = r.read_var_u32()?;
                        let f = module
                            .array_field(ti)
                            .ok_or(ValidateError::UndefinedType)?;
                        let len = stack.pop().ok_or(ValidateError::StackUnderflow)?;
                        if len != V::I32 {
                            return Err(ValidateError::TypeMismatch);
                        }
                        if sub == 0x06 {
                            let init = stack.pop().ok_or(ValidateError::StackUnderflow)?;
                            if !subtype_of(module, init, f.storage.unpacked()) {
                                return Err(ValidateError::TypeMismatch);
                            }
                        }
                        push(&mut stack, V::concrete_ref(false, RefHeap::Array, ti))?;
                    }
                    // array.new_fixed t n
                    0x08 => {
                        let ti = r.read_var_u32()?;
                        let n = r.read_var_u32()?;
                        let f = module
                            .array_field(ti)
                            .ok_or(ValidateError::UndefinedType)?;
                        for _ in 0..n {
                            let got = stack.pop().ok_or(ValidateError::StackUnderflow)?;
                            if !subtype_of(module, got, f.storage.unpacked()) {
                                return Err(ValidateError::TypeMismatch);
                            }
                        }
                        push(&mut stack, V::concrete_ref(false, RefHeap::Array, ti))?;
                    }
                    // ref.i31
                    0x1c => {
                        let got = stack.pop().ok_or(ValidateError::StackUnderflow)?;
                        if got != V::I32 {
                            return Err(ValidateError::TypeMismatch);
                        }
                        push(&mut stack, V::I31REF_NN)?;
                    }
                    // Everything else in the 0xFB family is not constant. Refused, not assumed.
                    _ => return Err(ValidateError::ConstantExpressionRequired),
                }
            }
            0x6a..=0x6c => {
                // i32 add/sub/mul (extended-const)
                gate(Some(Feature::ExtendedConst), features)?;
                let n = stack.len();
                if n < 2 || stack[n - 1] != V::I32 || stack[n - 2] != V::I32 {
                    return Err(ValidateError::TypeMismatch);
                }
                stack.pop();
            }
            0x7c..=0x7e => {
                // i64 add/sub/mul (extended-const)
                gate(Some(Feature::ExtendedConst), features)?;
                let n = stack.len();
                if n < 2 || stack[n - 1] != V::I64 || stack[n - 2] != V::I64 {
                    return Err(ValidateError::TypeMismatch);
                }
                stack.pop();
            }
            // `v128.const` — the SIMD prefix. The interpreter has evaluated this in a
            // const-expr since v0.6.5, so rejecting it here was a **false rejection**: a
            // module with a `v128` global validated as invalid despite running correctly.
            // Nothing but `v128.const` is constant in the 0xfd family.
            0xfd => {
                gate(Some(Feature::Simd), features)?;
                if r.read_var_u32()? != 0x0c {
                    return Err(ValidateError::ConstantExpressionRequired);
                }
                r.read_bytes(16)?;
                push(&mut stack, V::V128)?;
            }
            // The GC constant instructions (`struct.new`, `array.new*`, `ref.i31`, 0xfb …)
            // stay rejected, and the interpreter's `eval_const_expr` rejects them too — so
            // validator and engine agree. That is a missing feature, tracked in
            // `cmem/known-issues.md`, not a disagreement.
            _ => return Err(ValidateError::ConstantExpressionRequired),
        }
    }
    if stack.len() != 1 || !subtype_of(module, stack[0], expected) {
        return Err(ValidateError::TypeMismatch);
    }
    Ok(())
}

fn validate_function(
    module: &Module,
    ft: &FuncType,
    code: &Code,
    refs: Option<&[bool]>,
    features: &Features,
) -> ValidateResult<()> {
    // locals = parameters ++ declared locals (expanded from the run-length form). Sum first
    // (checked against the cap), then expand, so a huge run can't allocate first.
    let mut declared: u64 = 0;
    for l in &code.locals {
        declared += u64::from(l.count);
    }
    if declared + ft.params.len() as u64 > MAX_LOCALS {
        return Err(ValidateError::TooManyLocals);
    }
    let mut locals: Vec<V> = Vec::with_capacity(ft.params.len() + declared as usize);
    locals.extend_from_slice(&ft.params);
    for l in &code.locals {
        // A declared local is a value-type position like any other: `(local v128)` needs
        // SIMD even if the body never touches a vector instruction.
        gate_val_type(l.ty, features)?;
        locals.resize(locals.len() + l.count as usize, l.ty);
    }

    // Already decoded, at decode time — where a malformed instruction stream belongs. This used to
    // call `decode_body` here, which both put the error at the wrong stage and decoded every body
    // a second time.
    let instrs = &code.ir;

    // Local-init: params + defaultable locals start initialized; a non-nullable-ref local
    // starts uninitialized (function-references, §3.3.5).
    let n_params = ft.params.len();
    let local_init: Vec<bool> = locals
        .iter()
        .enumerate()
        .map(|(i, t)| i < n_params || !t.is_non_null_ref())
        .collect();

    let mut v = FuncValidator {
        module,
        features,
        refs,
        locals: &locals,
        results: &ft.results,
        local_init,
        vals: Vec::new(),
        ctrls: Vec::new(),
        body_len: instrs.len(),
    };
    // The whole body is an implicit block of type [] -> results.
    v.push_ctrl(FrameKind::Block, Vec::new(), ft.results.clone())?;
    for instr in instrs {
        // On failure, stamp the absolute module offset of the instruction that failed:
        // `Instr::offset` is body-relative (added at T9a#7) and `Code::body_offset` is the body's
        // own position, so their sum is the number wasmtime prints as "at offset N".
        v.step(instr).inspect_err(|_| {
            note_offset(code.body_offset.saturating_add(instr.offset));
        })?;
    }
    if !v.ctrls.is_empty() {
        return Err(ValidateError::ControlUnderflow); // missing `end`
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StackType {
    Val(V),
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Block,
    Loop,
    If,
    Else,
    /// A `try_table` (EH, exnref encoding) — block-shaped; its catch clauses are checked
    /// when the frame is pushed.
    TryTable,
    /// A legacy `try` — block-shaped until a `catch`/`catch_all` closes its body section.
    TryLegacy,
    /// A legacy `catch`/`catch_all` handler section.
    CatchLegacy,
}

struct Frame {
    kind: FrameKind,
    start: Vec<V>,
    end: Vec<V>,
    height: usize,
    is_unreachable: bool,
    init_snapshot: Vec<bool>,
}

struct FuncValidator<'a> {
    module: &'a Module,
    /// The proposals this engine accepts. Consulted once per instruction in
    /// [`FuncValidator::step`] via the `op_feature` table, so no arm can forget its gate.
    features: &'a Features,
    refs: Option<&'a [bool]>,
    locals: &'a [V],
    results: &'a [V],
    local_init: Vec<bool>,
    vals: Vec<StackType>,
    ctrls: Vec<Frame>,
    /// Instruction count of the body being checked. Bounds `array.new_fixed`'s operand
    /// count: `n` is an unvalidated `u32`, and in *unreachable* code `pop_expect` yields
    /// `Unknown` instead of underflowing, so an unbounded loop would spin up to 2^32 times
    /// on a tiny module. Every operand must be produced by at least one instruction, so a
    /// valid `n` can never exceed this — bounding by it cannot reject a valid module.
    body_len: usize,
}

impl<'a> FuncValidator<'a> {
    fn push_val_t(&mut self, t: V) {
        self.vals.push(StackType::Val(t));
    }
    fn push_val(&mut self, st: StackType) {
        self.vals.push(st);
    }
    fn push_vals(&mut self, ts: &[V]) {
        for &t in ts {
            self.push_val_t(t);
        }
    }

    fn pop_val(&mut self) -> ValidateResult<StackType> {
        let top = self.ctrls.last().ok_or(ValidateError::ControlUnderflow)?;
        if self.vals.len() == top.height {
            if top.is_unreachable {
                return Ok(StackType::Unknown);
            }
            return Err(ValidateError::StackUnderflow);
        }
        Ok(self.vals.pop().unwrap())
    }
    fn pop_expect(&mut self, expect: V) -> ValidateResult<StackType> {
        let actual = self.pop_val()?;
        if let StackType::Val(t) = actual {
            if !subtype_of(self.module, t, expect) {
                // The one place that knows BOTH types, which is what makes "expected i32, found
                // i64" possible at all — wasmtime's wording, captured here rather than
                // reconstructed by a caller that no longer has the operand.
                note_types(expect, t);
                return Err(ValidateError::TypeMismatch);
            }
        }
        Ok(actual)
    }
    fn pop_vals(&mut self, ts: &[V]) -> ValidateResult<()> {
        let mut i = ts.len();
        while i > 0 {
            i -= 1;
            self.pop_expect(ts[i])?;
        }
        Ok(())
    }
    fn pop_ref(&mut self) -> ValidateResult<StackType> {
        let st = self.pop_val()?;
        if let StackType::Val(v) = st {
            if !v.is_ref() {
                return Err(ValidateError::TypeMismatch);
            }
        }
        Ok(st)
    }

    fn require_memory(&self, index: u32) -> ValidateResult<()> {
        if index as usize >= self.module.memories.len() {
            return Err(ValidateError::MissingMemory);
        }
        Ok(())
    }
    fn mem_addr_ty(&self, index: u32) -> V {
        if self.module.memories[index as usize].limits.is64 {
            V::I64
        } else {
            V::I32
        }
    }
    fn check_mem_offset(&self, index: u32, offset: u64) -> ValidateResult<()> {
        if !self.module.memories[index as usize].limits.is64 && offset > u64::from(u32::MAX) {
            return Err(ValidateError::InvalidMemArgOffset);
        }
        Ok(())
    }

    fn top_unreachable(&self) -> bool {
        self.ctrls.last().is_some_and(|f| f.is_unreachable)
    }

    fn push_ctrl(&mut self, kind: FrameKind, start: Vec<V>, end: Vec<V>) -> ValidateResult<()> {
        if self.ctrls.len() >= MAX_CTRL_DEPTH {
            return Err(ValidateError::NestingTooDeep);
        }
        let height = self.vals.len();
        let init_snapshot = self.local_init.clone();
        self.push_vals(&start);
        self.ctrls.push(Frame {
            kind,
            start,
            end,
            height,
            is_unreachable: false,
            init_snapshot,
        });
        Ok(())
    }
    fn pop_ctrl(&mut self) -> ValidateResult<Frame> {
        let end = self
            .ctrls
            .last()
            .ok_or(ValidateError::ControlUnderflow)?
            .end
            .clone();
        self.pop_vals(&end)?;
        let frame = self.ctrls.pop().ok_or(ValidateError::ControlUnderflow)?;
        if self.vals.len() != frame.height {
            return Err(ValidateError::StackHeightMismatch);
        }
        Ok(frame)
    }
    fn set_unreachable(&mut self) {
        let top = self.ctrls.last_mut().unwrap();
        self.vals.truncate(top.height);
        top.is_unreachable = true;
    }

    fn label_types_at(&self, n: u32) -> ValidateResult<Vec<V>> {
        if n as usize >= self.ctrls.len() {
            return Err(ValidateError::UnknownLabel);
        }
        let frame = &self.ctrls[self.ctrls.len() - 1 - n as usize];
        Ok(if frame.kind == FrameKind::Loop {
            frame.start.clone()
        } else {
            frame.end.clone()
        })
    }

    fn local_at(&self, i: u32) -> ValidateResult<V> {
        self.locals
            .get(i as usize)
            .copied()
            .ok_or(ValidateError::UndefinedLocal)
    }
    /// §3.3.8: a tail call's callee results must satisfy the **enclosing function's** declared
    /// results — the callee returns to *our* caller, so our signature fixes the type.
    ///
    /// ⚠️ **This is SUBTYPING, not equality**, and getting that wrong is the same mistake this
    /// project has now made at four sites: `return_call_ref` shipped with an equality check and so
    /// refused valid modules — `(func (result (ref null $t)))` tail-calling a callee that returns
    /// `(ref $t)` is legal, because a non-nullable reference is a subtype of the nullable one.
    /// Equality is wrong in the *refusing* direction here, which is why nothing caught it: a
    /// rejected valid module is a failing conformance assertion, never a wrong answer.
    ///
    /// One authority for all three tail forms. They had three copies of the check, which by this
    /// project's own history means three copies of whatever is wrong with it.
    fn check_tail_results(&self, callee: &[V]) -> ValidateResult<()> {
        if callee.len() != self.results.len()
            || !callee
                .iter()
                .zip(self.results.iter())
                .all(|(&got, &want)| subtype_of(self.module, got, want))
        {
            return Err(ValidateError::TypeMismatch);
        }
        Ok(())
    }
    fn table_elem_type(&self, i: u32) -> ValidateResult<V> {
        self.module
            .tables
            .get(i as usize)
            .map(|t| t.element)
            .ok_or(ValidateError::UndefinedTable)
    }

    /// (pop, push) types of a block signature (§5.3.6).
    /// Look up a struct's field, bounds-checking the field index.
    fn struct_field(&self, ti: u32, fi: u32) -> ValidateResult<crate::module::FieldType> {
        let fields = self
            .module
            .struct_fields(ti)
            .ok_or(ValidateError::UndefinedType)?;
        fields
            .get(fi as usize)
            .copied()
            .ok_or(ValidateError::UndefinedField)
    }

    /// Every `want[i]` must be a subtype of `got[i]` (same length) — used to check that a
    /// catch handler's pushed values fit its target label.
    fn match_types(&self, want: &[V], got: &[V]) -> ValidateResult<()> {
        if want.len() != got.len() {
            return Err(ValidateError::TypeMismatch);
        }
        for (&w, &g) in want.iter().zip(got) {
            if !subtype_of(self.module, w, g) {
                return Err(ValidateError::TypeMismatch);
            }
        }
        Ok(())
    }

    /// A `try_table` catch clause branches to `lt` (its target label's types) carrying the
    /// tag's params (`catch`/`catch_ref`), plus an `exnref` for the `_ref` variants, and
    /// nothing at all for `catch_all`. Check those against `lt`.
    fn check_catch(&self, c: &opcode::Catch, lt: &[V]) -> ValidateResult<()> {
        match c.kind {
            opcode::CatchKind::Catch => {
                let ft = self
                    .module
                    .tag_type(c.tag)
                    .ok_or(ValidateError::UndefinedTag)?;
                self.match_types(&ft.params, lt)
            }
            opcode::CatchKind::CatchRef => {
                let ft = self
                    .module
                    .tag_type(c.tag)
                    .ok_or(ValidateError::UndefinedTag)?;
                if lt.len() != ft.params.len() + 1 {
                    return Err(ValidateError::TypeMismatch);
                }
                self.match_types(&ft.params, &lt[..ft.params.len()])?;
                if !subtype_of(self.module, V::EXNREF, lt[lt.len() - 1]) {
                    return Err(ValidateError::TypeMismatch);
                }
                Ok(())
            }
            opcode::CatchKind::CatchAll => {
                if lt.is_empty() {
                    Ok(())
                } else {
                    Err(ValidateError::TypeMismatch)
                }
            }
            opcode::CatchKind::CatchAllRef => {
                if lt.len() == 1 && subtype_of(self.module, V::EXNREF, lt[0]) {
                    Ok(())
                } else {
                    Err(ValidateError::TypeMismatch)
                }
            }
        }
    }

    fn block_sig(&self, bt: opcode::BlockType) -> ValidateResult<(Vec<V>, Vec<V>)> {
        match bt {
            opcode::BlockType::Empty => Ok((Vec::new(), Vec::new())),
            opcode::BlockType::Value(t) => {
                gate_val_type(t, self.features)?;
                Ok((Vec::new(), vec![t]))
            }
            opcode::BlockType::TypeIndex(i) => {
                let ft = self.module.func_sig(i).ok_or(ValidateError::UndefinedType)?;
                // A block signature that takes parameters, or returns more than one value,
                // is exactly what multi-value added — the single-valtype spelling above
                // covers everything WebAssembly 1.0 can express. (The referenced type's
                // own value types are gated with the type section.)
                if !ft.params.is_empty() || ft.results.len() > 1 {
                    gate(Some(Feature::MultiValue), self.features)?;
                }
                Ok((ft.params, ft.results))
            }
            // `(ref null? $t)` as a block result — the decoder leaves the index unresolved
            // because only the module's type section says which family `$t` belongs to.
            opcode::BlockType::ConcreteRef {
                nullable,
                type_index,
            } => {
                let t = ref_type_val_type(
                    self.module,
                    RefType {
                        nullable,
                        heap: HeapType::Concrete(type_index),
                    },
                )?;
                gate_val_type(t, self.features)?;
                Ok((Vec::new(), vec![t]))
            }
        }
    }

    fn step(&mut self, instr: &Instr) -> ValidateResult<()> {
        if self.ctrls.is_empty() {
            return Err(ValidateError::ControlUnderflow); // code after the final `end`
        }
        // The proposal gate, applied once for every instruction from the one `op_feature`
        // table — so an arm below cannot be added without inheriting its gate. The `0xFD`
        // family needs the sub-opcode to tell plain SIMD from relaxed SIMD, so it refines
        // the family-level answer.
        if self.features != &Features::all() {
            gate(op_feature(instr.op), self.features)?;
            match &instr.imm {
                Imm::Simd(s) => gate(Some(simd_sub_feature(s.sub)), self.features)?,
                // A typed `select` names its result type outright, so `(select (result
                // v128) …)` must answer to SIMD as well as to reference-types.
                Imm::SelectTypes(ts) => {
                    for &t in ts {
                        gate_val_type(t, self.features)?;
                    }
                }
                _ => {}
            }
        }
        match instr.op {
            Op::Unreachable => self.set_unreachable(),
            Op::Nop => {}

            Op::Block | Op::Loop | Op::If => {
                if instr.op == Op::If {
                    self.pop_expect(V::I32)?;
                }
                let bt = expect_block_type(&instr.imm)?;
                let (pop, push) = self.block_sig(bt)?;
                self.pop_vals(&pop)?;
                let kind = match instr.op {
                    Op::Block => FrameKind::Block,
                    Op::Loop => FrameKind::Loop,
                    _ => FrameKind::If,
                };
                self.push_ctrl(kind, pop, push)?;
            }
            Op::Else => {
                let frame = self.pop_ctrl()?;
                if frame.kind != FrameKind::If {
                    return Err(ValidateError::MismatchedElse);
                }
                self.local_init.copy_from_slice(&frame.init_snapshot);
                self.push_ctrl(FrameKind::Else, frame.start, frame.end)?;
            }

            // --- Exception handling: the exnref encoding ---
            Op::TryTable => {
                let Imm::TryTable(tt) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let (pop, push) = self.block_sig(tt.block_type)?;
                self.pop_vals(&pop)?;
                // Clone the clauses before pushing: the frame push borrows `self` mutably,
                // and the clauses live in the instruction's immediate.
                let catches = tt.catches.clone();
                self.push_ctrl(FrameKind::TryTable, pop, push)?;
                // Each clause's target label must accept exactly what the handler pushes:
                // the tag's params, plus an `exnref` for the `_ref` forms. Label indices
                // resolve with the try_table frame already on top.
                for c in &catches {
                    let lt = self.label_types_at(c.label)?;
                    self.check_catch(c, &lt)?;
                }
            }
            Op::Throw => {
                let ft = self
                    .module
                    .tag_type(expect_tag(&instr.imm)?)
                    .ok_or(ValidateError::UndefinedTag)?;
                if !ft.results.is_empty() {
                    return Err(ValidateError::InvalidTag); // tags never produce results
                }
                self.pop_vals(&ft.params)?; // the exception's operands
                self.set_unreachable(); // control transfers; the rest is dead
            }
            Op::ThrowRef => {
                self.pop_expect(V::EXNREF)?;
                self.set_unreachable();
            }

            // --- Exception handling: the legacy encoding ---
            // A `try` opens a block-typed frame; each `catch`/`catch_all` closes the
            // preceding section (which must produce the try's results) and opens a handler
            // starting from the tag's params; `end` closes the construct.
            Op::TryLegacy => {
                let bt = expect_block_type(&instr.imm)?;
                let (pop, push) = self.block_sig(bt)?;
                self.pop_vals(&pop)?;
                self.push_ctrl(FrameKind::TryLegacy, pop, push)?;
            }
            Op::CatchLegacy | Op::CatchAll => {
                let frame = self.pop_ctrl()?;
                if frame.kind != FrameKind::TryLegacy && frame.kind != FrameKind::CatchLegacy {
                    return Err(ValidateError::MismatchedCatch);
                }
                // The handler starts from the try's ENTRY init state: locals set in the body
                // (or in a prior handler) are not guaranteed on the path that reached this
                // catch via a thrown exception — the same rule as `else`.
                self.local_init.copy_from_slice(&frame.init_snapshot);
                let start: Vec<V> = if instr.op == Op::CatchLegacy {
                    let ft = self
                        .module
                        .tag_type(expect_tag(&instr.imm)?)
                        .ok_or(ValidateError::UndefinedTag)?;
                    if !ft.results.is_empty() {
                        return Err(ValidateError::InvalidTag);
                    }
                    ft.params // the caught exception's operands
                } else {
                    Vec::new() // catch_all binds nothing
                };
                self.push_ctrl(FrameKind::CatchLegacy, start, frame.end)?;
            }
            Op::Delegate => {
                // `delegate l` re-raises "at label l", which can SKIP the handlers of trys
                // between this one and the target. The frozen oracle does not implement that
                // routing (its interpreter traps on it) and rejects `delegate` here rather
                // than accept a construct it cannot correctly execute. wasmrt matches, so
                // the validator and the interpreter agree. Every other legacy construct —
                // `try`/`catch`/`catch_all`/`rethrow` — is fully supported.
                return Err(ValidateError::UnsupportedValidation);
            }
            Op::Rethrow => {
                // Re-raise the exception caught `l` levels out: `l` must resolve, and
                // control transfers, so the rest of the block is dead.
                self.label_types_at(expect_label(&instr.imm)?)?;
                self.set_unreachable();
            }
            Op::End => {
                let frame = self.pop_ctrl()?;
                // An `if` closed without `else` has an implicit identity else branch: its
                // params and results must match.
                if frame.kind == FrameKind::If && frame.start != frame.end {
                    return Err(ValidateError::TypeMismatch);
                }
                self.local_init.copy_from_slice(&frame.init_snapshot);
                self.push_vals(&frame.end);
            }

            Op::Br => {
                let lt = self.label_types_at(expect_label(&instr.imm)?)?;
                self.pop_vals(&lt)?;
                self.set_unreachable();
            }
            Op::BrIf => {
                self.pop_expect(V::I32)?;
                let lt = self.label_types_at(expect_label(&instr.imm)?)?;
                self.pop_vals(&lt)?;
                self.push_vals(&lt);
            }
            Op::BrTable => {
                self.pop_expect(V::I32)?;
                let Imm::BrTable(bt) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let default_lt = self.label_types_at(bt.default)?;
                for &l in &bt.labels {
                    let lt = self.label_types_at(l)?;
                    // Arity must agree across every target — §3.3.5.8 asks for ONE operand
                    // sequence that satisfies them all. Their element types need NOT be
                    // related to each other: `(unreachable) (br_table 0 1 …)` between an
                    // `f32` block and an `f64` one is valid, because the operands are then
                    // bottom, which is a subtype of both. Comparing labels pairwise instead
                    // of against the operands rejected exactly that (`meet-bottom`).
                    if lt.len() != default_lt.len() {
                        return Err(ValidateError::TypeMismatch);
                    }
                    // Check the operands against this label, then put back EXACTLY what was
                    // popped. Pushing the label's own types instead widened the stack, so a
                    // later target that is *narrower* than an earlier one saw the widened
                    // type and failed — §3.3.5.8 asks each label type to be a supertype of
                    // the OPERANDS, not of the other labels.
                    let mut actual = Vec::with_capacity(lt.len());
                    let mut i = lt.len();
                    while i > 0 {
                        i -= 1;
                        actual.push(self.pop_expect(lt[i])?);
                    }
                    for st in actual.into_iter().rev() {
                        self.push_val(st);
                    }
                }
                self.pop_vals(&default_lt)?;
                self.set_unreachable();
            }
            Op::Return => {
                let results = self.results.to_vec();
                self.pop_vals(&results)?;
                self.set_unreachable();
            }

            Op::Call => {
                let ft = self
                    .module
                    .func_type(expect_func(&instr.imm)?)
                    .ok_or(ValidateError::UndefinedFunc)?;
                self.pop_vals(&ft.params)?;
                self.push_vals(&ft.results);
            }
            Op::CallIndirect => {
                let Imm::CallIndirect(ci) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let tet = self.table_elem_type(ci.table)?;
                if !subtype_of(self.module, tet, V::FUNCREF) {
                    return Err(ValidateError::TypeMismatch);
                }
                let ft = self
                    .module
                    .func_sig(ci.type_index)
                    .ok_or(ValidateError::UndefinedType)?;
                self.pop_expect(V::I32)?;
                self.pop_vals(&ft.params)?;
                self.push_vals(&ft.results);
            }
            // §3.3.8 tail calls. Two differences from the non-tail twin, and both matter:
            // the callee's results must satisfy **this function's declared results** (the tail call
            // returns to *our* caller, so its type is fixed by our signature and not by what is on
            // the stack), and the instruction ends the block — hence `set_unreachable` rather than
            // pushing results.
            Op::ReturnCall => {
                let ft = self
                    .module
                    .func_type(expect_func(&instr.imm)?)
                    .ok_or(ValidateError::UndefinedFunc)?;
                self.pop_vals(&ft.params)?;
                self.check_tail_results(&ft.results)?;
                self.set_unreachable();
            }
            Op::ReturnCallIndirect => {
                let Imm::CallIndirect(ci) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let tet = self.table_elem_type(ci.table)?;
                if !subtype_of(self.module, tet, V::FUNCREF) {
                    return Err(ValidateError::TypeMismatch);
                }
                let ft = self
                    .module
                    .func_sig(ci.type_index)
                    .ok_or(ValidateError::UndefinedType)?;
                self.pop_expect(V::I32)?;
                self.pop_vals(&ft.params)?;
                self.check_tail_results(&ft.results)?;
                self.set_unreachable();
            }
            Op::CallRef => {
                let ti = expect_func(&instr.imm)?;
                let ft = self.module.func_sig(ti).ok_or(ValidateError::UndefinedType)?;
                self.pop_expect(V::concrete_ref(true, RefHeap::Func, ti))?;
                self.pop_vals(&ft.params)?;
                self.push_vals(&ft.results);
            }
            Op::ReturnCallRef => {
                let ti = expect_func(&instr.imm)?;
                let ft = self.module.func_sig(ti).ok_or(ValidateError::UndefinedType)?;
                self.pop_expect(V::concrete_ref(true, RefHeap::Func, ti))?;
                self.pop_vals(&ft.params)?;
                self.check_tail_results(&ft.results)?;
                self.set_unreachable();
            }

            Op::Drop => {
                self.pop_val()?;
            }
            Op::Select => {
                self.pop_expect(V::I32)?;
                let t1 = self.pop_val()?;
                let t2 = self.pop_val()?;
                if is_ref_stack(t1) || is_ref_stack(t2) {
                    return Err(ValidateError::TypeMismatch);
                }
                let rt = match (t1, t2) {
                    (StackType::Unknown, _) => t2,
                    (_, StackType::Unknown) => t1,
                    (StackType::Val(a), StackType::Val(b)) => {
                        if a == b {
                            t1
                        } else {
                            return Err(ValidateError::TypeMismatch);
                        }
                    }
                };
                self.push_val(rt);
            }
            Op::SelectT => {
                let Imm::SelectTypes(tys) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                if tys.len() != 1 {
                    return Err(ValidateError::TypeMismatch);
                }
                let t = tys[0];
                self.pop_expect(V::I32)?;
                self.pop_expect(t)?;
                self.pop_expect(t)?;
                self.push_val_t(t);
            }

            Op::TableGet => {
                let et = self.table_elem_type(expect_table(&instr.imm)?)?;
                self.pop_expect(V::I32)?;
                self.push_val_t(et);
            }
            Op::TableSet => {
                let et = self.table_elem_type(expect_table(&instr.imm)?)?;
                self.pop_expect(et)?;
                self.pop_expect(V::I32)?;
            }
            Op::TableSize => {
                self.table_elem_type(expect_table(&instr.imm)?)?;
                self.push_val_t(V::I32);
            }
            Op::TableGrow => {
                let et = self.table_elem_type(expect_table(&instr.imm)?)?;
                self.pop_expect(V::I32)?;
                self.pop_expect(et)?;
                self.push_val_t(V::I32);
            }
            Op::TableFill => {
                let et = self.table_elem_type(expect_table(&instr.imm)?)?;
                self.pop_expect(V::I32)?;
                self.pop_expect(et)?;
                self.pop_expect(V::I32)?;
            }
            Op::TableInit => {
                let Imm::TableInit { elem, table } = instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let tet = self.table_elem_type(table)?;
                if elem as usize >= self.module.elements.len() {
                    return Err(ValidateError::UndefinedElem);
                }
                if !subtype_of(self.module, self.module.elements[elem as usize].elem_type, tet) {
                    return Err(ValidateError::TypeMismatch);
                }
                self.pop_expect(V::I32)?;
                self.pop_expect(V::I32)?;
                self.pop_expect(V::I32)?;
            }
            Op::TableCopy => {
                let Imm::TableCopy { dst, src } = instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let dt = self.table_elem_type(dst)?;
                let st = self.table_elem_type(src)?;
                if !subtype_of(self.module, st, dt) {
                    return Err(ValidateError::TypeMismatch);
                }
                self.pop_expect(V::I32)?;
                self.pop_expect(V::I32)?;
                self.pop_expect(V::I32)?;
            }
            Op::ElemDrop => {
                if expect_elem(&instr.imm)? as usize >= self.module.elements.len() {
                    return Err(ValidateError::UndefinedElem);
                }
            }

            Op::MemoryFill => {
                let mi = expect_mem_index(&instr.imm)?;
                self.require_memory(mi)?;
                let at = self.mem_addr_ty(mi);
                self.pop_expect(at)?;
                self.pop_expect(V::I32)?; // fill byte
                self.pop_expect(at)?;
            }
            Op::MemoryCopy => {
                let Imm::MemCopy { dst, src } = instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                self.require_memory(dst)?;
                self.require_memory(src)?;
                let dt = self.mem_addr_ty(dst);
                let st = self.mem_addr_ty(src);
                let nt = if dt == V::I64 && st == V::I64 {
                    V::I64
                } else {
                    V::I32
                };
                self.pop_expect(nt)?;
                self.pop_expect(st)?;
                self.pop_expect(dt)?;
            }
            Op::MemoryInit => {
                let Imm::MemInit { data, mem } = instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                self.require_memory(mem)?;
                if data as usize >= self.module.data.len() {
                    return Err(ValidateError::UndefinedData);
                }
                self.pop_expect(V::I32)?; // n
                self.pop_expect(V::I32)?; // src offset into the segment
                self.pop_expect(self.mem_addr_ty(mem))?; // dst address
            }
            Op::DataDrop => {
                if expect_data(&instr.imm)? as usize >= self.module.data.len() {
                    return Err(ValidateError::UndefinedData);
                }
            }

            Op::RefNull => {
                let vt = ref_type_val_type(
                    self.module,
                    RefType {
                        nullable: true,
                        heap: expect_ref_type(&instr.imm)?,
                    },
                )?;
                // `ref.null` is the one non-GC instruction that names an arbitrary heap
                // type, so `ref.null any` must answer to GC even though the opcode itself
                // is reference-types.
                gate_val_type(vt, self.features)?;
                self.push_val_t(vt);
            }
            Op::RefIsNull => {
                if let StackType::Val(v) = self.pop_val()? {
                    if !v.is_ref() {
                        return Err(ValidateError::TypeMismatch);
                    }
                }
                self.push_val_t(V::I32);
            }
            Op::RefFunc => {
                let fi = expect_func(&instr.imm)?;
                if self.module.func_type(fi).is_none() {
                    return Err(ValidateError::UndefinedFunc);
                }
                if let Some(set) = self.refs {
                    if !set.get(fi as usize).copied().unwrap_or(false) {
                        return Err(ValidateError::UndeclaredFuncRef);
                    }
                }
                if let Some(ti) = self.module.func_type_index(fi) {
                    self.push_val_t(V::concrete_ref(false, RefHeap::Func, ti));
                } else {
                    self.push_val_t(V::FUNCREF_NN);
                }
            }
            Op::RefEq => {
                self.pop_expect(V::EQREF)?;
                self.pop_expect(V::EQREF)?;
                self.push_val_t(V::I32);
            }
            Op::RefI31 => {
                self.pop_expect(V::I32)?;
                self.push_val_t(V::I31REF_NN);
            }

            // --- WasmGC: struct objects ---
            // A concrete `(ref $t)` is popped as the CONCRETE type, never the family head:
            // popping `structref` would let ANY struct reference satisfy ANY `struct.*`, so
            // `struct.get $b 0` on a `(ref $a)` could reinterpret one field type as another.
            // `subtype_of` already walks the declared supertype chain for concrete pairs.
            Op::StructNew => {
                let ti = expect_gc_type(&instr.imm)?;
                let fields = self
                    .module
                    .struct_fields(ti)
                    .ok_or(ValidateError::UndefinedType)?;
                for f in fields.iter().rev() {
                    // operands are pushed field 0 first → pop in reverse
                    self.pop_expect(f.storage.unpacked())?;
                }
                self.push_val_t(V::concrete_ref(false, RefHeap::Struct, ti));
            }
            Op::StructNewDefault => {
                let ti = expect_gc_type(&instr.imm)?;
                let fields = self
                    .module
                    .struct_fields(ti)
                    .ok_or(ValidateError::UndefinedType)?;
                if fields.iter().any(|f| f.storage.unpacked().is_non_null_ref()) {
                    return Err(ValidateError::TypeMismatch); // not defaultable
                }
                self.push_val_t(V::concrete_ref(false, RefHeap::Struct, ti));
            }
            Op::StructGet | Op::StructGetS | Op::StructGetU => {
                let (ti, fi) = expect_gc_field(&instr.imm)?;
                let field = self.struct_field(ti, fi)?;
                require_packing(instr.op == Op::StructGet, field.storage)?;
                self.pop_expect(V::concrete_ref(true, RefHeap::Struct, ti))?;
                self.push_val_t(field.storage.unpacked());
            }
            Op::StructSet => {
                let (ti, fi) = expect_gc_field(&instr.imm)?;
                let field = self.struct_field(ti, fi)?;
                if !field.mutable {
                    return Err(ValidateError::ImmutableField);
                }
                self.pop_expect(field.storage.unpacked())?;
                self.pop_expect(V::concrete_ref(true, RefHeap::Struct, ti))?;
            }

            // --- WasmGC: array objects ---
            Op::ArrayNew => {
                let ti = expect_gc_type(&instr.imm)?;
                let f = self
                    .module
                    .array_field(ti)
                    .ok_or(ValidateError::UndefinedType)?;
                self.pop_expect(V::I32)?; // length
                self.pop_expect(f.storage.unpacked())?; // init value
                self.push_val_t(V::concrete_ref(false, RefHeap::Array, ti));
            }
            Op::ArrayNewDefault => {
                let ti = expect_gc_type(&instr.imm)?;
                let f = self
                    .module
                    .array_field(ti)
                    .ok_or(ValidateError::UndefinedType)?;
                if f.storage.unpacked().is_non_null_ref() {
                    return Err(ValidateError::TypeMismatch); // not defaultable
                }
                self.pop_expect(V::I32)?; // length
                self.push_val_t(V::concrete_ref(false, RefHeap::Array, ti));
            }
            Op::ArrayNewFixed => {
                let Imm::GcTypeN { type_index, n } = instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let f = self
                    .module
                    .array_field(type_index)
                    .ok_or(ValidateError::UndefinedType)?;
                if n as usize > self.body_len {
                    return Err(ValidateError::StackUnderflow); // see `body_len`
                }
                for _ in 0..n {
                    self.pop_expect(f.storage.unpacked())?;
                }
                self.push_val_t(V::concrete_ref(false, RefHeap::Array, type_index));
            }
            Op::ArrayGet | Op::ArrayGetS | Op::ArrayGetU => {
                let ti = expect_gc_type(&instr.imm)?;
                let f = self
                    .module
                    .array_field(ti)
                    .ok_or(ValidateError::UndefinedType)?;
                require_packing(instr.op == Op::ArrayGet, f.storage)?;
                self.pop_expect(V::I32)?; // index
                self.pop_expect(V::concrete_ref(true, RefHeap::Array, ti))?;
                self.push_val_t(f.storage.unpacked());
            }
            Op::ArraySet => {
                let ti = expect_gc_type(&instr.imm)?;
                let f = self
                    .module
                    .array_field(ti)
                    .ok_or(ValidateError::UndefinedType)?;
                if !f.mutable {
                    return Err(ValidateError::ImmutableField);
                }
                self.pop_expect(f.storage.unpacked())?; // value
                self.pop_expect(V::I32)?; // index
                self.pop_expect(V::concrete_ref(true, RefHeap::Array, ti))?;
            }
            Op::ArrayLen => {
                self.pop_expect(V::ARRAYREF)?;
                self.push_val_t(V::I32);
            }

            // --- SIMD (the `0xFD` v128 family) ---
            Op::Simd => {
                let Imm::Simd(s) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let sg = simd_sig(s.sub);
                if opcode::simd_is_memory_op(s.sub) {
                    // A memory-touching SIMD op needs a memory to exist, its memarg index in
                    // range (multi-memory), and its alignment within the natural maximum.
                    let mi = s.mem.memory;
                    self.require_memory(mi)?;
                    if s.mem.alignment > opcode::simd_natural_align_log2(s.sub) {
                        return Err(ValidateError::InvalidAlignment);
                    }
                    self.check_mem_offset(mi, s.mem.offset)?;
                    // memory64: the address operand is the memory's index type, not the
                    // `i32` baked into `simd_sig`. `pop[0]` is always the address — pop the
                    // trailing v128 value(s) top-first, then it.
                    let at = self.mem_addr_ty(mi);
                    let mut k = sg.pop.len();
                    while k > 1 {
                        k -= 1;
                        self.pop_expect(sg.pop[k])?;
                    }
                    self.pop_expect(at)?;
                    self.push_vals(sg.push);
                } else {
                    self.pop_vals(sg.pop)?;
                    self.push_vals(sg.push);
                }
            }

            // --- Threads / atomics (the `0xFE` family) ---
            Op::Atomic => {
                let Imm::Atomic(a) = &instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let sub = a.sub;
                if sub == 0x03 {
                    return Ok(()); // atomic.fence: no memory, no operands
                }
                // Every other atomic op touches memory and MUST be naturally aligned — the
                // alignment is exact here, not a maximum as it is for scalar/SIMD access.
                self.require_memory(a.mem.memory)?;
                if a.mem.alignment != opcode::atomic_natural_align_log2(sub) {
                    return Err(ValidateError::InvalidAlignment);
                }
                self.check_mem_offset(a.mem.memory, a.mem.offset)?;
                // memory64: the ADDRESS operand (the deepest one) takes the memory's type.
                let adt = self.mem_addr_ty(a.mem.memory);
                match sub {
                    0x00 => {
                        // notify: [addr, count] -> [i32]
                        self.pop_expect(V::I32)?;
                        self.pop_expect(adt)?;
                        self.push_val_t(V::I32);
                    }
                    0x01 => {
                        // wait32: [addr, i32, i64] -> [i32]
                        self.pop_expect(V::I64)?;
                        self.pop_expect(V::I32)?;
                        self.pop_expect(adt)?;
                        self.push_val_t(V::I32);
                    }
                    0x02 => {
                        // wait64: [addr, i64, i64] -> [i32]
                        self.pop_expect(V::I64)?;
                        self.pop_expect(V::I64)?;
                        self.pop_expect(adt)?;
                        self.push_val_t(V::I32);
                    }
                    0x10..=0x16 => {
                        // atomic load: [addr] -> [T]
                        self.pop_expect(adt)?;
                        self.push_val_t(atomic_val_type(sub));
                    }
                    0x17..=0x1d => {
                        // atomic store: [addr, T] -> []
                        self.pop_expect(atomic_val_type(sub))?;
                        self.pop_expect(adt)?;
                    }
                    0x1e..=0x47 => {
                        // rmw: [addr, T] -> [T]
                        let t = atomic_val_type(sub);
                        self.pop_expect(t)?;
                        self.pop_expect(adt)?;
                        self.push_val_t(t);
                    }
                    0x48..=0x4e => {
                        // cmpxchg: [addr, expected, replacement] -> [T]
                        let t = atomic_val_type(sub);
                        self.pop_expect(t)?;
                        self.pop_expect(t)?;
                        self.pop_expect(adt)?;
                        self.push_val_t(t);
                    }
                    _ => return Err(ValidateError::UnsupportedValidation),
                }
            }

            // --- WasmGC: casts ---
            // Spec: `ref.test rt : [rt'] -> [i32]` with `rt <: rt'` — operand and target
            // must share a TOP type, so `ref.test (ref func)` on an `externref` is invalid.
            Op::RefTest | Op::RefCastOp => {
                let target = ref_type_val_type(self.module, expect_ref_cast(&instr.imm)?)?;
                self.pop_expect(target.ref_heap().top().val_type(true))?;
                if instr.op == Op::RefTest {
                    self.push_val_t(V::I32);
                } else {
                    self.push_val_t(target);
                }
            }
            // The label carries `[t* rt]`; the operand is `[t* src]`. `br_on_cast` branches
            // when the ref matches `dst` and falls through otherwise; `br_on_cast_fail` is
            // the mirror. `dst` must be a subtype of `src` (a downcast).
            Op::BrOnCast | Op::BrOnCastFail => {
                let Imm::BrCast { label, src, dst } = instr.imm else {
                    return Err(ValidateError::UnsupportedValidation);
                };
                let src_vt = ref_type_val_type(self.module, src)?;
                let dst_vt = ref_type_val_type(self.module, dst)?;
                if !subtype_of(self.module, dst_vt, src_vt) {
                    return Err(ValidateError::TypeMismatch);
                }
                let lt = self.label_types_at(label)?;
                if lt.is_empty() {
                    return Err(ValidateError::TypeMismatch);
                }
                // What the branch carries: `dst` for br_on_cast (it fires on a match),
                // `src` for br_on_cast_fail (it fires on a miss).
                let carried = if instr.op == Op::BrOnCast {
                    dst_vt
                } else {
                    src_vt
                };
                if !subtype_of(self.module, carried, lt[lt.len() - 1]) {
                    return Err(ValidateError::TypeMismatch);
                }
                let prefix = lt[..lt.len() - 1].to_vec(); // t*
                self.pop_expect(src_vt)?; // the ref operand (top)
                self.pop_vals(&prefix)?;
                self.push_vals(&prefix);
                // Fall-through: `src` for br_on_cast — but when the cast target is NULLABLE
                // a null would have branched, so the fall-through ref is non-null.
                // `br_on_cast_fail` falls through with the narrowed `dst`.
                self.push_val_t(if instr.op == Op::BrOnCast {
                    if dst_vt.is_non_null_ref() {
                        src_vt
                    } else {
                        src_vt.non_null()
                    }
                } else {
                    dst_vt
                });
            }
            Op::I31GetS | Op::I31GetU => {
                self.pop_expect(V::I31REF)?;
                self.push_val_t(V::I32);
            }
            Op::RefAsNonNull => match self.pop_ref()? {
                StackType::Val(v) => self.push_val_t(v.non_null()),
                StackType::Unknown => self.push_val(StackType::Unknown),
            },
            Op::BrOnNull => {
                let r = self.pop_ref()?;
                let lt = self.label_types_at(expect_label(&instr.imm)?)?;
                self.pop_vals(&lt)?;
                self.push_vals(&lt);
                match r {
                    StackType::Val(v) => self.push_val_t(v.non_null()),
                    StackType::Unknown => self.push_val(StackType::Unknown),
                }
            }
            Op::BrOnNonNull => {
                let lt = self.label_types_at(expect_label(&instr.imm)?)?;
                if lt.is_empty() || !lt[lt.len() - 1].is_ref() {
                    return Err(ValidateError::TypeMismatch);
                }
                self.pop_vals(&lt)?;
                self.push_vals(&lt);
                if let StackType::Val(v) = self.pop_ref()? {
                    if !subtype_of(self.module, v, lt[lt.len() - 1].nullable()) {
                        return Err(ValidateError::TypeMismatch);
                    }
                }
            }

            Op::LocalGet => {
                let i = expect_local(&instr.imm)?;
                let t = self.local_at(i)?;
                if t.is_non_null_ref() && !self.local_init[i as usize] && !self.top_unreachable() {
                    return Err(ValidateError::UninitializedLocal);
                }
                self.push_val_t(t);
            }
            Op::LocalSet => {
                let i = expect_local(&instr.imm)?;
                let t = self.local_at(i)?;
                self.pop_expect(t)?;
                self.local_init[i as usize] = true;
            }
            Op::LocalTee => {
                let i = expect_local(&instr.imm)?;
                let t = self.local_at(i)?;
                self.pop_expect(t)?;
                self.local_init[i as usize] = true;
                self.push_val_t(t);
            }
            Op::GlobalGet => {
                let g = self
                    .module
                    .globals
                    .get(expect_global(&instr.imm)? as usize)
                    .ok_or(ValidateError::UndefinedGlobal)?;
                self.push_val_t(g.content);
            }
            Op::GlobalSet => {
                let g = *self
                    .module
                    .globals
                    .get(expect_global(&instr.imm)? as usize)
                    .ok_or(ValidateError::UndefinedGlobal)?;
                if !g.mutable {
                    return Err(ValidateError::ImmutableGlobal);
                }
                self.pop_expect(g.content)?;
            }

            // Loads/stores, memory.size/grow, and the numeric/compare/convert/const ops.
            _ => match &instr.imm {
                Imm::Mem(ma) => {
                    self.require_memory(ma.memory)?;
                    if ma.alignment > opcode::natural_align_log2(instr.op) {
                        return Err(ValidateError::InvalidAlignment);
                    }
                    self.check_mem_offset(ma.memory, ma.offset)?;
                    let s = simple_sig(instr.op).ok_or(ValidateError::UnsupportedValidation)?;
                    let at = self.mem_addr_ty(ma.memory);
                    // pop trailing value(s) top-first, then the address as `at`
                    let mut k = s.pop.len();
                    while k > 1 {
                        k -= 1;
                        self.pop_expect(s.pop[k])?;
                    }
                    self.pop_expect(at)?;
                    self.push_vals(s.push);
                }
                Imm::MemIndex(mi) => {
                    self.require_memory(*mi)?;
                    let at = self.mem_addr_ty(*mi);
                    if instr.op == Op::MemoryGrow {
                        self.pop_expect(at)?;
                    }
                    self.push_val_t(at);
                }
                _ => {
                    let s = simple_sig(instr.op).ok_or(ValidateError::UnsupportedValidation)?;
                    self.pop_vals(s.pop)?;
                    self.push_vals(s.push);
                }
            },
        }
        Ok(())
    }
}

// --- Immediate extractors (decode guarantees op↔imm correspondence) ----------

fn expect_label(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Label(l) = imm {
        Ok(*l)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_func(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Func(f) = imm {
        Ok(*f)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_local(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Local(l) = imm {
        Ok(*l)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_global(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Global(g) = imm {
        Ok(*g)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_table(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Table(t) = imm {
        Ok(*t)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_elem(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Elem(e) = imm {
        Ok(*e)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_data(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Data(d) = imm {
        Ok(*d)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_mem_index(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::MemIndex(m) = imm {
        Ok(*m)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_gc_type(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::GcType(t) = imm {
        Ok(*t)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_gc_field(imm: &Imm) -> ValidateResult<(u32, u32)> {
    if let Imm::GcField { type_index, field } = imm {
        Ok((*type_index, *field))
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_ref_cast(imm: &Imm) -> ValidateResult<RefType> {
    if let Imm::RefCast(rt) = imm {
        Ok(*rt)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}

/// A packed (`i8`/`i16`) field must be read with the sign-aware `*_get_s`/`*_get_u`; an
/// unpacked one must be read with the plain `*.get`. `plain` says which form this op is.
fn require_packing(plain: bool, storage: crate::module::StorageType) -> ValidateResult<()> {
    let is_packed = !matches!(storage, crate::module::StorageType::Val(_));
    if plain == is_packed {
        return Err(ValidateError::TypeMismatch);
    }
    Ok(())
}

fn expect_tag(imm: &Imm) -> ValidateResult<u32> {
    if let Imm::Tag(t) = imm {
        Ok(*t)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_block_type(imm: &Imm) -> ValidateResult<opcode::BlockType> {
    if let Imm::BlockType(bt) = imm {
        Ok(*bt)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}
fn expect_ref_type(imm: &Imm) -> ValidateResult<HeapType> {
    if let Imm::RefType(ht) = imm {
        Ok(*ht)
    } else {
        Err(ValidateError::UnsupportedValidation)
    }
}

// --- Subtyping + signatures --------------------------------------------------

fn is_ref_stack(st: StackType) -> bool {
    matches!(st, StackType::Val(v) if v.is_ref())
}

/// The value type of a cast/ref target reference type.
fn ref_type_val_type(module: &Module, rt: RefType) -> ValidateResult<V> {
    let head = module.ref_head(rt.heap)?;
    Ok(match rt.heap {
        HeapType::Concrete(ti) => V::concrete_ref(rt.nullable, head, ti),
        _ => head.val_type(rt.nullable),
    })
}

/// Is `sub` a subtype of `sup` (operand matching)? Identical types match; reference
/// subtyping follows the WasmGC hierarchy on the heap type combined with nullability
/// (`(ref t) <: (ref null t)`).
fn subtype_of(module: &Module, sub: V, sup: V) -> bool {
    if sub == sup {
        return true;
    }
    if !sub.is_ref() || !sup.is_ref() {
        return false;
    }
    if sup.is_non_null_ref() && !sub.is_non_null_ref() {
        return false;
    }
    if sub.is_concrete() && sup.is_concrete() {
        return module.is_subtype(sub.concrete_index(), sup.concrete_index());
    }
    if sub.is_concrete() {
        return sub.ref_heap().is_subtype_of(sup.ref_heap());
    }
    if sup.is_concrete() {
        return sub.ref_heap() == RefHeap::None;
    }
    sub.ref_heap().is_subtype_of(sup.ref_heap())
}

struct Sig {
    pop: &'static [V],
    push: &'static [V],
}
const fn sig(pop: &'static [V], push: &'static [V]) -> Sig {
    Sig { pop, push }
}

const EMPTY: &[V] = &[];
const I32_1: &[V] = &[V::I32];
const I32_2: &[V] = &[V::I32, V::I32];
const I64_1: &[V] = &[V::I64];
const I64_2: &[V] = &[V::I64, V::I64];
const F32_1: &[V] = &[V::F32];
const F32_2: &[V] = &[V::F32, V::F32];
const F64_1: &[V] = &[V::F64];
const F64_2: &[V] = &[V::F64, V::F64];
const V128_1: &[V] = &[V::V128];
const V128_2: &[V] = &[V::V128, V::V128];
const V128_3: &[V] = &[V::V128, V::V128, V::V128];
const V128_SHIFT: &[V] = &[V::V128, V::I32];
const ADDR_V128: &[V] = &[V::I32, V::V128];
const V128_I32: &[V] = &[V::V128, V::I32];
const V128_I64: &[V] = &[V::V128, V::I64];
const V128_F32: &[V] = &[V::V128, V::F32];
const V128_F64: &[V] = &[V::V128, V::F64];
const STORE_I64: &[V] = &[V::I32, V::I64];
const STORE_F32: &[V] = &[V::I32, V::F32];
const STORE_F64: &[V] = &[V::I32, V::F64];

/// Value-type signature of a `0xFD` SIMD sub-opcode. Memory ops list an `i32` address as
/// `pop[0]`; the caller substitutes the memory's real index type (memory64).
///
/// The arity here must match the interpreter's, since both sides agree that a `v128` is a
/// single operand (wasmrt's 128-bit value slot — see `interp.rs`).
fn simd_sig(sub: u32) -> Sig {
    match sub {
        0x00..=0x0a | 0x5c | 0x5d => sig(I32_1, V128_1), // loads: addr -> v128
        0x0b => sig(ADDR_V128, EMPTY),                   // v128.store
        0x54..=0x57 => sig(ADDR_V128, V128_1),           // load lane
        0x58..=0x5b => sig(ADDR_V128, EMPTY),            // store lane
        0x0c => sig(EMPTY, V128_1),                      // v128.const
        0x0d | 0x0e => sig(V128_2, V128_1),              // shuffle / swizzle
        0x0f..=0x11 => sig(I32_1, V128_1),               // i8/i16/i32 splat
        0x12 => sig(I64_1, V128_1),                      // i64x2.splat
        0x13 => sig(F32_1, V128_1),                      // f32x4.splat
        0x14 => sig(F64_1, V128_1),                      // f64x2.splat
        0x15 | 0x16 | 0x18 | 0x19 | 0x1b => sig(V128_1, I32_1), // extract_lane -> i32
        0x1d => sig(V128_1, I64_1),
        0x1f => sig(V128_1, F32_1),
        0x21 => sig(V128_1, F64_1),
        0x17 | 0x1a | 0x1c => sig(V128_I32, V128_1), // replace_lane
        0x1e => sig(V128_I64, V128_1),
        0x20 => sig(V128_F32, V128_1),
        0x22 => sig(V128_F64, V128_1),
        0x23..=0x4c => sig(V128_2, V128_1), // comparisons
        0x4d => sig(V128_1, V128_1),        // v128.not
        0x4e..=0x51 => sig(V128_2, V128_1), // and / andnot / or / xor
        // bitselect + relaxed madd/nmadd/laneselect/dot_add
        0x52 | 0x105..=0x10c | 0x113 => sig(V128_3, V128_1),
        0x53 | 0x63 | 0x83 | 0xa3 | 0xc3 => sig(V128_1, I32_1), // any_true / all_true
        0x64 | 0x84 | 0xa4 | 0xc4 => sig(V128_1, I32_1),        // bitmask
        0x6b..=0x6d | 0x8b..=0x8d | 0xab..=0xad | 0xcb..=0xcd => sig(V128_SHIFT, V128_1),
        // unary v128 -> v128: abs/neg/popcnt, sqrt, ceil/floor/trunc/nearest, extend
        // low/high, extadd_pairwise, int<->float convert, trunc_sat, promote/demote
        // (incl. relaxed_trunc).
        0x60..=0x62
        | 0x67..=0x6a
        | 0x74
        | 0x75
        | 0x7a
        | 0x7c..=0x7f
        | 0x80
        | 0x81
        | 0x87..=0x8a
        | 0x94
        | 0xa0
        | 0xa1
        | 0xa7..=0xaa
        | 0xc0
        | 0xc1
        | 0xc7..=0xca
        | 0x5e
        | 0x5f
        | 0xe0
        | 0xe1
        | 0xe3
        | 0xec
        | 0xed
        | 0xef
        | 0xf8..=0xff
        | 0x101..=0x104 => sig(V128_1, V128_1),
        // default: binary lane arithmetic (incl. relaxed swizzle/min/max/q15/dot)
        _ => sig(V128_2, V128_1),
    }
}

/// The value type a `0xFE` atomic sub-opcode loads/stores/exchanges.
fn atomic_val_type(sub: u32) -> V {
    match sub {
        0x10 | 0x12 | 0x13 | 0x17 | 0x19 | 0x1a => V::I32, // i32 loads/stores (full/8/16)
        0x11 | 0x14 | 0x15 | 0x16 | 0x18 | 0x1b | 0x1c | 0x1d => V::I64, // i64 loads/stores
        // rmw/cmpxchg in groups of 7: [i32.full, i64.full, i32.8, i32.16, i64.8, i64.16,
        // i64.32] — positions 0, 2, 3 are i32; the rest i64.
        _ => match (sub.wrapping_sub(0x1e)) % 7 {
            0 | 2 | 3 => V::I32,
            _ => V::I64,
        },
    }
}

/// Fixed value-type signature for the numeric / comparison / conversion / const / load /
/// store / memory opcodes. `None` for opcodes handled specially in [`FuncValidator::step`].
fn simple_sig(op: Op) -> Option<Sig> {
    Some(match op as u8 {
        // Comparisons
        0x45 => sig(I32_1, I32_1),        // i32.eqz
        0x46..=0x4f => sig(I32_2, I32_1), // i32 compares
        0x50 => sig(I64_1, I32_1),        // i64.eqz
        0x51..=0x5a => sig(I64_2, I32_1),
        0x5b..=0x60 => sig(F32_2, I32_1),
        0x61..=0x66 => sig(F64_2, I32_1),
        // Numeric
        0x67..=0x69 => sig(I32_1, I32_1),
        0x6a..=0x78 => sig(I32_2, I32_1),
        0x79..=0x7b => sig(I64_1, I64_1),
        0x7c..=0x8a => sig(I64_2, I64_1),
        0x8b..=0x91 => sig(F32_1, F32_1),
        0x92..=0x98 => sig(F32_2, F32_1),
        0x99..=0x9f => sig(F64_1, F64_1),
        0xa0..=0xa6 => sig(F64_2, F64_1),
        // Conversions
        0xa7 => sig(I64_1, I32_1),
        0xa8 | 0xa9 => sig(F32_1, I32_1),
        0xaa | 0xab => sig(F64_1, I32_1),
        0xac | 0xad => sig(I32_1, I64_1),
        0xae | 0xaf => sig(F32_1, I64_1),
        0xb0 | 0xb1 => sig(F64_1, I64_1),
        // Saturating truncation (internal tags for 0xFC 0x00–0x07).
        0xc5 | 0xc6 => sig(F32_1, I32_1),
        0xc7 | 0xc8 => sig(F64_1, I32_1),
        0xc9 | 0xca => sig(F32_1, I64_1),
        0xcb | 0xcc => sig(F64_1, I64_1),
        0xb2 | 0xb3 => sig(I32_1, F32_1),
        0xb4 | 0xb5 => sig(I64_1, F32_1),
        0xb6 => sig(F64_1, F32_1),
        0xb7 | 0xb8 => sig(I32_1, F64_1),
        0xb9 | 0xba => sig(I64_1, F64_1),
        0xbb => sig(F32_1, F64_1),
        0xbc => sig(F32_1, I32_1),
        0xbd => sig(F64_1, I64_1),
        0xbe => sig(I32_1, F32_1),
        0xbf => sig(I64_1, F64_1),
        // Sign extension
        0xc0 | 0xc1 => sig(I32_1, I32_1),
        0xc2..=0xc4 => sig(I64_1, I64_1),
        // Constants
        0x41 => sig(EMPTY, I32_1),
        0x42 => sig(EMPTY, I64_1),
        0x43 => sig(EMPTY, F32_1),
        0x44 => sig(EMPTY, F64_1),
        // Loads: [i32 addr] -> [value]
        0x28 | 0x2c | 0x2d | 0x2e | 0x2f => sig(I32_1, I32_1),
        0x29 | 0x30 | 0x31 | 0x32 | 0x33 | 0x34 | 0x35 => sig(I32_1, I64_1),
        0x2a => sig(I32_1, F32_1),
        0x2b => sig(I32_1, F64_1),
        // Stores: [i32 addr, value] -> []
        0x36 | 0x3a | 0x3b => sig(I32_2, EMPTY),
        0x37 | 0x3c | 0x3d | 0x3e => sig(STORE_I64, EMPTY),
        0x38 => sig(STORE_F32, EMPTY),
        0x39 => sig(STORE_F64, EMPTY),
        // Memory
        0x3f => sig(EMPTY, I32_1), // memory.size
        0x40 => sig(I32_1, I32_1), // memory.grow
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::decode;

    fn m(rest: &[u8]) -> Vec<u8> {
        let mut v = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        v.extend_from_slice(rest);
        v
    }

    /// The validator and the interpreter must agree on what a constant expression is. When
    /// they disagree the engine is not unsafe — validation runs first — but a *valid* module
    /// gets refused, which is the harder bug to notice. `v128.const` was exactly that from
    /// v0.6.5 (when the interpreter learned it) until 2026-08-05.
    #[test]
    fn a_v128_global_initializer_is_a_constant_expression() {
        // globals: one v128 global initialized to v128.const 0…0.
        let mut sec = vec![0x01, 0x7b, 0x00, 0xfd, 0x0c];
        sec.extend_from_slice(&[0u8; 16]);
        sec.push(0x0b);
        let mut bytes = vec![0x06, u8::try_from(sec.len()).unwrap()];
        bytes.extend_from_slice(&sec);
        let md = decode(&m(&bytes)).expect("decode");
        assert!(
            validate(&md).is_ok(),
            "a v128 global must validate — the interpreter already evaluates it"
        );
    }

    /// The 0xfd prefix carries ~230 opcodes and only `v128.const` is constant; the rest must
    /// still be refused, or the check above would have opened the door to all of them.
    #[test]
    fn a_non_const_simd_op_is_still_not_a_constant_expression() {
        // v128.not (0xfd 0x4d) in a global initializer.
        let sec = vec![0x01, 0x7b, 0x00, 0xfd, 0x4d, 0x0b];
        let mut bytes = vec![0x06, u8::try_from(sec.len()).unwrap()];
        bytes.extend_from_slice(&sec);
        let Ok(md) = decode(&m(&bytes)) else { return };
        assert!(validate(&md).is_err(), "only v128.const is constant");
    }

    #[test]
    fn validates_well_typed_add() {
        // Export (7) before code (10), the order §5.5.2 fixes — this fixture had them reversed
        // until the decoder started enforcing it.
        let bytes = m(&[
            0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x07, 0x01, 0x03, b'a', b'd', b'd', 0x00, 0x00,
            0x0a, 0x0b, 0x01, 0x09, 0x01, 0x01, 0x7f, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn rejects_stack_underflow() {
        // type ()->() ; one func ; body: i32.add end
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x05, 0x01, 0x03, 0x00, 0x6a, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::StackUnderflow));
    }

    #[test]
    fn rejects_result_type_mismatch() {
        // type ()->(i32) ; body: f32.const 0 end
        let bytes = m(&[
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x09, 0x01, 0x07, 0x00, 0x43, 0x00, 0x00, 0x00, 0x00, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::TypeMismatch));
    }

    // --- GC constant expressions, §3.3.11 (T9a#5, 2026-08-08) ---

    /// The six GC forms that are constant. Validator and interpreter accept exactly the same set, so
    /// they cannot disagree about what a constant expression is — the failure mode a previous defect
    /// had (`v128.const` evaluated fine but validated as invalid, a false rejection).
    #[test]
    fn the_gc_constant_forms_are_accepted_in_a_global_initializer() {
        for src in [
            r#"(module (type $s (struct (field f32)))
                (global (ref $s) (struct.new $s (f32.const 1))))"#,
            r#"(module (type $s (struct (field f32)))
                (global (ref $s) (struct.new_default $s)))"#,
            r#"(module (type $v (array f32))
                (global (ref $v) (array.new $v (f32.const 1) (i32.const 3))))"#,
            r#"(module (type $v (array f32))
                (global (ref $v) (array.new_default $v (i32.const 3))))"#,
            r#"(module (type $v (array f32))
                (global (ref $v) (array.new_fixed $v 2 (f32.const 1) (f32.const 2))))"#,
            r#"(module (global (ref i31) (ref.i31 (i32.const 2))))"#,
        ] {
            assert_eq!(v(src), Ok(()), "should be a valid constant expression: {src}");
        }
    }

    /// …and the typing is real, not a rubber stamp: operand types, arity and the *result* type are all
    /// checked, and a non-constant `0xFB` form is still refused.
    #[test]
    fn gc_constant_expressions_are_still_type_checked() {
        // Wrong field type.
        assert!(matches!(
            v(r#"(module (type $s (struct (field f32)))
                 (global (ref $s) (struct.new $s (i64.const 1))))"#),
            Err(ValidateError::TypeMismatch)
        ));
        // `array.new` wants an i32 length.
        assert!(matches!(
            v(r#"(module (type $v (array f32))
                 (global (ref $v) (array.new $v (f32.const 1) (f32.const 3))))"#),
            Err(ValidateError::TypeMismatch)
        ));
        // `ref.i31` wants an i32.
        assert!(matches!(
            v(r#"(module (global (ref i31) (ref.i31 (f32.const 2))))"#),
            Err(ValidateError::TypeMismatch)
        ));
        // A GC op that is NOT constant stays refused — the set is enumerated, not opened up.
        assert!(matches!(
            v(r#"(module (type $s (struct (field f32)))
                 (global f32 (struct.get $s 0 (ref.null $s))))"#),
            Err(ValidateError::ConstantExpressionRequired | ValidateError::TypeMismatch)
        ));
    }

    /// The interpreter must actually *produce* the value, not merely permit it — a validator that
    /// accepts what the evaluator rejects is the same disagreement in the other direction.
    #[test]
    fn gc_constant_expressions_evaluate_at_instantiation() {
        let src = r#"(module
            (type $s (struct (field i32) (field i32)))
            (type $v (array i32))
            (global $g (ref $s) (struct.new $s (i32.const 7) (i32.const 9)))
            (global $a (ref $v) (array.new $v (i32.const 5) (i32.const 3)))
            (global $i (ref i31) (ref.i31 (i32.const 42)))
            (func (export "field") (result i32) (struct.get $s 1 (global.get $g)))
            (func (export "len") (result i32) (array.len (global.get $a)))
            (func (export "elem") (result i32) (array.get $v (global.get $a) (i32.const 2)))
            (func (export "i31") (result i32) (i31.get_s (global.get $i))))"#;
        let md = decode(&crate::wat::assemble(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(validate(&md), Ok(()));
        let mut inst = crate::interp::Instance::new(md).expect("instantiate");
        for (name, want) in [("field", 9), ("len", 3), ("elem", 5), ("i31", 42)] {
            assert_eq!(
                crate::interp::as_i32(inst.invoke(name, &[]).unwrap()[0]),
                want,
                "{name}"
            );
        }
    }

    // --- declared subtyping, §3.4.5 (2026-08-08) ---

    /// Assemble, decode and validate `src`, returning the verdict.
    fn v(src: &str) -> ValidateResult<()> {
        let bytes = crate::wat::assemble(src.as_bytes()).expect("assemble");
        validate(&decode(&bytes).expect("decode"))
    }

    /// **Finality.** A type is final unless declared `(sub …)`, and a final type cannot be extended.
    /// Nothing enforced this, so any type could be named as any other's supertype.
    #[test]
    fn a_final_type_cannot_be_a_supertype() {
        // Bare `(func)` / `(struct)` are shorthand for `sub final ϵ`.
        assert_eq!(
            v("(module (type $t (func)) (type $s (sub $t (func))))"),
            Err(ValidateError::SubType)
        );
        assert_eq!(
            v("(module (type $t (struct)) (type $s (sub $t (struct))))"),
            Err(ValidateError::SubType)
        );
        // Explicit `sub final`.
        assert_eq!(
            v("(module (type $t (sub final (func))) (type $s (sub $t (func))))"),
            Err(ValidateError::SubType)
        );
        // A type may be open, extended, and the extension made final — after which it closes.
        assert_eq!(
            v("(module (type $t (sub (func))) (type $s (sub final $t (func)))
                 (type $u (sub $s (func))))"),
            Err(ValidateError::SubType)
        );
        // And the same hierarchy without the `final` is valid — so the check is not simply refusing
        // every declared supertype.
        assert_eq!(
            v("(module (type $t (sub (func))) (type $s (sub $t (func)))
                 (type $u (sub $s (func))))"),
            Ok(())
        );
    }

    /// **The assembler was making open types final.** `(sub …)` with no supertype emitted a *bare*
    /// composite type, which is the shorthand for `sub final ϵ` — so the module it produced was not
    /// the module the text described, and a valid hierarchy became invalid. Caught only because the
    /// finality check above started reading the flag.
    #[test]
    fn sub_with_no_supertype_assembles_as_open_not_final() {
        assert_eq!(
            v("(module (type $b (sub (struct))) (type $d (sub $b (struct))))"),
            Ok(())
        );
        // The distinction is real in the bytes: `(struct)` alone is final and refuses the extension.
        assert_eq!(
            v("(module (type $b (struct)) (type $d (sub $b (struct))))"),
            Err(ValidateError::SubType)
        );
    }

    #[test]
    fn a_declared_supertype_of_a_different_kind_is_refused() {
        for src in [
            "(module (type $a (sub (array i32))) (type $s (sub $a (struct))))",
            "(module (type $s (sub (struct))) (type $a (sub $s (array i32))))",
            "(module (type $f (sub (func (param i32) (result i32)))) (type $s (sub $f (struct))))",
        ] {
            assert_eq!(v(src), Err(ValidateError::SubType), "should refuse: {src}");
        }
    }

    /// Struct extension appends; it never drops, reorders, or changes a field's mutability.
    #[test]
    fn struct_subtyping_may_only_append_matching_fields() {
        // Appending is fine, and a shared immutable field may narrow (covariant).
        assert_eq!(
            v("(module (type $b (sub (struct (field i32))))
                 (type $d (sub $b (struct (field i32) (field i64)))))"),
            Ok(())
        );
        // Dropping a field is not.
        assert_eq!(
            v("(module (type $b (sub (struct (field i32) (field i64))))
                 (type $d (sub $b (struct (field i32)))))"),
            Err(ValidateError::SubType)
        );
        // A shared field's type must still match.
        assert_eq!(
            v("(module (type $b (sub (struct (field i32))))
                 (type $d (sub $b (struct (field i64)))))"),
            Err(ValidateError::SubType)
        );
        // Mutability is part of the field type, in both directions.
        assert_eq!(
            v("(module (type $b (sub (struct (field i32))))
                 (type $d (sub $b (struct (field (mut i32))))))"),
            Err(ValidateError::SubType)
        );
        assert_eq!(
            v("(module (type $b (sub (struct (field (mut i32)))))
                 (type $d (sub $b (struct (field i32)))))"),
            Err(ValidateError::SubType)
        );
        // A packed field matches only the identical packing.
        assert_eq!(
            v("(module (type $b (sub (struct (field i8))))
                 (type $d (sub $b (struct (field i16)))))"),
            Err(ValidateError::SubType)
        );
        assert_eq!(
            v("(module (type $b (sub (struct (field i8)))) (type $d (sub $b (struct (field i8)))))"),
            Ok(())
        );
    }

    /// Function subtyping: **parameters contravariant, results covariant**. Getting a direction
    /// backwards is silent — it would accept a hierarchy that lets `call_ref` pass the wrong type —
    /// so both directions are asserted, not just the accepting one.
    #[test]
    fn func_subtyping_is_contravariant_in_params_and_covariant_in_results() {
        // `$s <: $t` requires $t's param to be a subtype of $s's (accept more), and $s's result to
        // be a subtype of $t's (promise more).
        assert_eq!(
            v("(module (type $x (sub (struct))) (type $y (sub $x (struct)))
                 (type $t (sub (func (param (ref $y)) (result anyref))))
                 (type $s (sub $t (func (param (ref $x)) (result (ref any))))))"),
            Ok(())
        );
        // Narrowing a parameter is the wrong direction.
        assert_eq!(
            v("(module (type $x (sub (struct))) (type $y (sub $x (struct)))
                 (type $t (sub (func (param (ref $x)))))
                 (type $s (sub $t (func (param (ref $y))))))"),
            Err(ValidateError::SubType)
        );
        // Arity must match on both sides.
        assert_eq!(
            v("(module (type $t (sub (func (param i32)))) (type $s (sub $t (func))))"),
            Err(ValidateError::SubType)
        );
        assert_eq!(
            v("(module (type $t (sub (func (result i32)))) (type $s (sub $t (func))))"),
            Err(ValidateError::SubType)
        );
    }

    /// Two structurally identical rec groups are **one type** (§3.1.4), so `$f1` and `$f2` here are
    /// the same type and the extension is valid.
    ///
    /// This used to be pinned as a *limitation* — the check could not tell the pair apart, so it
    /// accepted rather than refused. Canonicalisation decides it properly, and the approximation
    /// that stood in for it is gone.
    #[test]
    fn structurally_equal_rec_groups_are_the_same_type() {
        assert_eq!(
            v("(module
                 (rec (type $f1 (sub (func))) (type $s1 (sub (struct (field (ref $f1))))))
                 (rec (type $f2 (sub (func))) (type $s2 (sub (struct (field (ref $f2))))))
                 (type (sub $s2 (struct (field (ref $f1))))))"),
            Ok(())
        );
    }

    /// The other direction, which is the one that makes canonicalisation a *check* and not a
    /// rubber stamp: a rec group whose member refers to **its own** member is a different type from
    /// one referring **outward** to an identically-shaped type, even though the two spell out the
    /// same bytes locally. `type-rec.wast` asserts this and wasmrt used to get it wrong in both
    /// directions — accepting the invalid module, because it compared indices.
    #[test]
    fn a_self_referential_rec_group_differs_from_one_referring_outward() {
        // Group B's struct points at group A's `$f1`, not at its own `$f2`, so `$f2` is NOT `$f1`
        // and a `(ref $f1)` global cannot hold a `$f2` function.
        assert_eq!(
            v("(module
                 (rec (type $f1 (func)) (type (struct (field (ref $f1)))))
                 (rec (type $f2 (func)) (type (struct (field (ref $f1)))))
                 (func $f (type $f2))
                 (global (ref $f1) (ref.func $f)))"),
            Err(ValidateError::TypeMismatch)
        );
        // Written self-referentially in both groups, they ARE the same type and it is valid.
        assert_eq!(
            v("(module
                 (rec (type $f1 (func)) (type (struct (field (ref $f1)))))
                 (rec (type $f2 (func)) (type (struct (field (ref $f2)))))
                 (func $f (type $f2))
                 (global (ref $f1) (ref.func $f)))"),
            Ok(())
        );
    }

    /// `call_indirect`'s runtime check is **type identity with subtyping**, not signature shape — the
    /// third site to need that. Both functions here are `(func)`, so a param/result comparison lets the
    /// call through; the types differ because rec-group membership differs, so it must trap.
    #[test]
    fn call_indirect_traps_on_a_same_shaped_but_different_type() {
        let src = r#"(module
            (rec (type $a1 (sub (func))) (type $a2 (sub $a1 (func))))
            (rec (type $b1 (sub (func))) (type $b2 (sub $a1 (func))))
            (func $f (type $b1))
            (table 1 funcref) (elem (i32.const 0) $f)
            (func (export "go") (call_indirect (type $a1) (i32.const 0))))"#;
        let bytes = crate::wat::assemble(src.as_bytes()).expect("assemble");
        let md = decode(&bytes).expect("decode");
        assert_eq!(validate(&md), Ok(()), "the module itself is valid");
        let mut inst = crate::interp::Instance::new(md).expect("instantiate");
        assert_eq!(
            inst.invoke("go", &[]).err(),
            Some(crate::interp::Trap::IndirectTypeMismatch),
            "$b1 and $a1 are both (func) but are different types"
        );
    }

    /// The other direction: a callee whose type is a **subtype** of the declared one must be allowed
    /// through (§4.4.8), which equality refused.
    #[test]
    fn call_indirect_accepts_a_subtype_of_the_declared_type() {
        let src = r#"(module
            (type $t0 (sub (func)))
            (type $t1 (sub $t0 (func)))
            (func $f (type $t1))
            (table 1 funcref) (elem (i32.const 0) $f)
            (func (export "go") (call_indirect (type $t0) (i32.const 0))))"#;
        let bytes = crate::wat::assemble(src.as_bytes()).expect("assemble");
        let md = decode(&bytes).expect("decode");
        let mut inst = crate::interp::Instance::new(md).expect("instantiate");
        assert!(inst.invoke("go", &[]).is_ok(), "$t1 <: $t0, so the call is legal");
    }

    /// A rec group is the unit of identity, so **group size is part of the type**: a two-member group
    /// of `(func)`s is not the same type as a standalone `(func)`. The assembler was flattening every
    /// `(rec …)` into singletons, which silently changed what its output meant.
    #[test]
    fn rec_group_membership_is_part_of_type_identity() {
        let two = crate::wat::assemble(
            b"(module (rec (type $a (func)) (type $b (func))) (type $c (func)))",
        )
        .expect("assemble");
        let md = decode(&two).expect("decode");
        // $a and $b are members of one group of two; $c is its own singleton. None are equal.
        assert!(!md.types_equal(0, 2), "a two-member group is not a singleton");
        assert!(!md.types_equal(0, 1), "distinct positions in a group are distinct types");
        // The emitted binary must actually contain the rec wrapper (0x4e), or none of the above is
        // being tested — it was absent before, which is how the flattening went unnoticed.
        assert!(two.contains(&0x4e), "the type section must carry the rec group");
    }

    #[test]
    fn rejects_function_code_count_mismatch() {
        // The decoder now refuses this at the right stage (§5.5.13 is malformed, not invalid), so
        // it can no longer be reached through `decode` — pinned there by
        // `module::tests::rejects_function_code_count_mismatch_at_decode`.
        //
        // The validator's arm stays and is exercised by building the mismatch directly, because
        // `Module`'s fields are public: `validate` can be handed a module nobody decoded. A check
        // that is only harmless "because that case cannot occur" becomes a bug the moment it can —
        // the third-order lesson from T8.
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
        ]);
        let mut md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Ok(()));
        md.code.clear();
        assert_eq!(validate(&md), Err(ValidateError::CountMismatch));
    }

    #[test]
    fn resource_cap_rejects_huge_locals() {
        // (func) with one locals run of count 0xFFFFFFFF.
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x0a, 0x01, 0x08, 0x01, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x7f, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::TooManyLocals));
    }

    #[test]
    fn resource_cap_rejects_deep_nesting() {
        let depth = MAX_CTRL_DEPTH + 1;
        let mut body = vec![0x00u8]; // no locals
        for _ in 0..depth {
            body.extend_from_slice(&[0x02, 0x40]); // block (empty type)
        }
        body.resize(body.len() + depth, 0x0b); // matching `end`s
        body.push(0x0b); // function end
        let mut code = vec![0x01u8]; // one body
        write_uleb(&mut code, body.len() as u32);
        code.extend_from_slice(&body);
        let mut section = vec![0x0au8];
        write_uleb(&mut section, code.len() as u32);
        section.extend_from_slice(&code);
        let mut rest = vec![0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00];
        rest.extend_from_slice(&section);
        let md = decode(&m(&rest)).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::NestingTooDeep));
    }

    #[test]
    fn memory_ops_require_a_memory() {
        // (func (result i32) memory.size) with no memory -> MissingMemory.
        let no_mem = m(&[
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x3f, 0x00, 0x0b, // memory.size 0 ; end
        ]);
        let md = decode(&no_mem).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::MissingMemory));

        // Same, but with a memory declared -> valid.
        let with_mem = m(&[
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x05, 0x03, 0x01, 0x00, 0x01, // memory: count 1, flag 0, min 1
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x3f, 0x00, 0x0b,
        ]);
        let md = decode(&with_mem).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn rejects_duplicate_export() {
        // two exports both named "a" of the one function -> DuplicateExport.
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x09, 0x02, 0x01, b'a', 0x00, 0x00, 0x01, b'a', 0x00, 0x00,
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::DuplicateExport));
    }

    #[test]
    fn rejects_reserved_alignment() {
        // (func (param i32) i32.load align=4 (>2 natural) drop) -> InvalidAlignment.
        // body: local.get 0 ; i32.load align=4 offset=0 ; drop ; end
        let bytes = m(&[
            0x01, 0x05, 0x01, 0x60, 0x01, 0x7f, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x05, 0x03, 0x01, 0x00, 0x01, // a memory so the op is otherwise valid
            0x0a, 0x0a, 0x01, 0x08, 0x00, 0x20, 0x00, 0x28, 0x04, 0x00, 0x1a, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::InvalidAlignment));
    }

    // --- memory64 typing ---

    /// Section 5 declaring one memory64 memory (limits flag `0x04` = i64 index), min 1.
    const MEM64: [u8; 5] = [0x05, 0x03, 0x01, 0x04, 0x01];

    #[test]
    fn mem64_address_must_be_i64() {
        // (func) i32.const 0 ; i32.load ; drop  — an i32 address on a 64-bit memory.
        let mut rest = vec![0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00];
        rest.extend_from_slice(&MEM64);
        rest.extend_from_slice(&[
            0x0a, 0x0a, 0x01, 0x08, 0x00, 0x41, 0x00, 0x28, 0x02, 0x00, 0x1a, 0x0b,
        ]);
        let md = decode(&m(&rest)).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::TypeMismatch));

        // Same body with an i64 address (0x42 = i64.const) -> valid.
        let mut ok = vec![0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00];
        ok.extend_from_slice(&MEM64);
        ok.extend_from_slice(&[
            0x0a, 0x0a, 0x01, 0x08, 0x00, 0x42, 0x00, 0x28, 0x02, 0x00, 0x1a, 0x0b,
        ]);
        let md = decode(&m(&ok)).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn mem64_size_yields_i64() {
        // (func (result i32) memory.size) over a 64-bit memory -> the result is i64.
        let mut rest = vec![0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00];
        rest.extend_from_slice(&MEM64);
        rest.extend_from_slice(&[0x0a, 0x06, 0x01, 0x04, 0x00, 0x3f, 0x00, 0x0b]);
        let md = decode(&m(&rest)).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::TypeMismatch));

        // Declaring the result as i64 (0x7e) -> valid.
        let mut ok = vec![0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7e, 0x03, 0x02, 0x01, 0x00];
        ok.extend_from_slice(&MEM64);
        ok.extend_from_slice(&[0x0a, 0x06, 0x01, 0x04, 0x00, 0x3f, 0x00, 0x0b]);
        let md = decode(&m(&ok)).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn rejects_memarg_offset_above_u32_on_32bit_memory() {
        // i32.load offset=2^32 against a 32-bit memory -> InvalidMemArgOffset.
        let body = [
            0x00, 0x41, 0x00, 0x28, 0x02, 0x80, 0x80, 0x80, 0x80, 0x10, 0x1a, 0x0b,
        ];
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x05, 0x03, 0x01, 0x00, 0x01, // 32-bit memory, min 1
            0x0a, 0x0e, 0x01, 0x0c, body[0], body[1], body[2], body[3], body[4], body[5],
            body[6], body[7], body[8], body[9], body[10], body[11],
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::InvalidMemArgOffset));

        // The same offset on a 64-bit memory is legal (bounds are a runtime concern).
        let mut body64 = body;
        body64[1] = 0x42; // i64.const address
        let mut ok = vec![0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00];
        ok.extend_from_slice(&MEM64);
        ok.extend_from_slice(&[0x0a, 0x0e, 0x01, 0x0c]);
        ok.extend_from_slice(&body64);
        let md = decode(&m(&ok)).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn mem64_data_offset_must_be_i64() {
        // An active data segment on a 64-bit memory needs an i64 offset const-expr.
        let mut rest = MEM64.to_vec();
        // data: 1 segment, flag 0 (active, memory 0), (i32.const 0), 1 byte
        rest.extend_from_slice(&[0x0b, 0x07, 0x01, 0x00, 0x41, 0x00, 0x0b, 0x01, 0x2a]);
        let md = decode(&m(&rest)).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::TypeMismatch));

        // i64.const (0x42) -> valid.
        let mut ok = MEM64.to_vec();
        ok.extend_from_slice(&[0x0b, 0x07, 0x01, 0x00, 0x42, 0x00, 0x0b, 0x01, 0x2a]);
        let md = decode(&m(&ok)).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn rejects_memory64_limits_above_the_type_ceiling() {
        // min = 2^48 + 1 pages exceeds the 64-bit memory ceiling (2^48).
        let bytes = m(&[
            0x05, 0x09, 0x01, 0x04, 0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(validate(&md), Err(ValidateError::InvalidLimits));

        // Exactly 2^48 is at the ceiling -> accepted.
        let ok = m(&[
            0x05, 0x09, 0x01, 0x04, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40,
        ]);
        let md = decode(&ok).unwrap();
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn rejects_64bit_table() {
        // Tables are 32-bit-indexed in the frozen oracle: an i64 table type is malformed.
        // table section: 1 table, funcref (0x70), limits flag 0x04 (i64), min 1.
        let bytes = m(&[0x04, 0x04, 0x01, 0x70, 0x04, 0x01]);
        assert_eq!(decode(&bytes), Err(crate::types::DecodeError::MalformedFlag));
    }

    // --- the previously deferred typing arms: SIMD / atomics / GC / EH ---

    /// Assemble a module from `(section_id, content)` pairs (content < 128 bytes).
    fn asm(sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut v = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        for (id, c) in sections {
            v.push(*id);
            v.push(c.len() as u8);
            v.extend_from_slice(c);
        }
        v
    }
    /// A code section holding one body.
    fn code1(body: &[u8]) -> Vec<u8> {
        let mut c = vec![0x01u8, body.len() as u8];
        c.extend_from_slice(body);
        c
    }
    /// `() -> ()` with one memory (min 1) and `body`.
    fn mem_mod(body: &[u8]) -> Vec<u8> {
        asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x00]),
            (3, vec![0x01, 0x00]),
            (5, vec![0x01, 0x00, 0x01]),
            (10, code1(body)),
        ])
    }
    fn check(bytes: &[u8]) -> ValidateResult<()> {
        validate(&decode(bytes).unwrap())
    }

    #[test]
    fn simd_typing_accepts_and_checks_alignment() {
        // i32.const 0 ; v128.load align=4 ; drop  — align 4 (16 bytes) is the natural max.
        let ok = [0x00, 0x41, 0x00, 0xfd, 0x00, 0x04, 0x00, 0x1a, 0x0b];
        assert_eq!(check(&mem_mod(&ok)), Ok(()));

        // align=5 exceeds the natural alignment of v128.load.
        let over = [0x00, 0x41, 0x00, 0xfd, 0x00, 0x05, 0x00, 0x1a, 0x0b];
        assert_eq!(
            check(&mem_mod(&over)),
            Err(ValidateError::InvalidAlignment)
        );

        // A narrower SIMD load has a smaller natural maximum: v128.load8_splat (0x07) is 1
        // byte, so align=1 is already too much.
        let lane_over = [0x00, 0x41, 0x00, 0xfd, 0x07, 0x01, 0x00, 0x1a, 0x0b];
        assert_eq!(
            check(&mem_mod(&lane_over)),
            Err(ValidateError::InvalidAlignment)
        );
    }

    #[test]
    fn simd_memory_op_requires_a_memory() {
        // The same v128.load in a module with NO memory must be rejected, not silently
        // accepted the way an unchecked SIMD arm would.
        let body = [0x00, 0x41, 0x00, 0xfd, 0x00, 0x04, 0x00, 0x1a, 0x0b];
        let no_mem = asm(&[
            (1, vec![0x01, 0x60, 0x00, 0x00]),
            (3, vec![0x01, 0x00]),
            (10, code1(&body)),
        ]);
        assert_eq!(check(&no_mem), Err(ValidateError::MissingMemory));
    }

    #[test]
    fn simd_typing_rejects_a_wrong_operand_type() {
        // i32.const 0 ; i32.add — feeding an i32 where i32x4.add wants two v128s.
        // (i32x4.add = 0xfd 0xae 0x01)
        let bad = [
            0x00, 0x41, 0x00, 0x41, 0x00, 0xfd, 0xae, 0x01, 0x1a, 0x0b,
        ];
        assert_eq!(check(&mem_mod(&bad)), Err(ValidateError::TypeMismatch));
    }

    #[test]
    fn atomic_alignment_must_be_exact() {
        // i32.atomic.load requires align=2 EXACTLY — unlike a scalar load, where a smaller
        // alignment is merely a hint and stays valid.
        let ok = [0x00, 0x41, 0x00, 0xfe, 0x10, 0x02, 0x00, 0x1a, 0x0b];
        assert_eq!(check(&mem_mod(&ok)), Ok(()));

        let under = [0x00, 0x41, 0x00, 0xfe, 0x10, 0x01, 0x00, 0x1a, 0x0b];
        assert_eq!(
            check(&mem_mod(&under)),
            Err(ValidateError::InvalidAlignment)
        );

        // The scalar counterpart with the same under-alignment stays valid.
        let scalar = [0x00, 0x41, 0x00, 0x28, 0x01, 0x00, 0x1a, 0x0b];
        assert_eq!(check(&mem_mod(&scalar)), Ok(()));
    }

    #[test]
    fn atomic_rmw_typing() {
        // i32.atomic.rmw.add : [addr, i32] -> [i32]
        let ok = [
            0x00, 0x41, 0x00, 0x41, 0x05, 0xfe, 0x1e, 0x02, 0x00, 0x1a, 0x0b,
        ];
        assert_eq!(check(&mem_mod(&ok)), Ok(()));

        // …fed an i64 value instead of an i32.
        let bad = [
            0x00, 0x41, 0x00, 0x42, 0x05, 0xfe, 0x1e, 0x02, 0x00, 0x1a, 0x0b,
        ];
        assert_eq!(check(&mem_mod(&bad)), Err(ValidateError::TypeMismatch));
    }

    /// `() -> ()` plus a struct type `$1` whose single i32 field has mutability `mutable`.
    fn struct_mod(body: &[u8], mutable: u8) -> Vec<u8> {
        asm(&[
            (
                1,
                vec![0x02, 0x60, 0x00, 0x00, 0x5f, 0x01, 0x7f, mutable],
            ),
            (3, vec![0x01, 0x00]),
            (10, code1(body)),
        ])
    }

    #[test]
    fn gc_struct_typing() {
        // struct.new_default $1 ; struct.get $1 0 ; drop
        let ok = [
            0x00, 0xfb, 0x01, 0x01, 0xfb, 0x02, 0x01, 0x00, 0x1a, 0x0b,
        ];
        assert_eq!(check(&struct_mod(&ok, 0x01)), Ok(()));

        // Field index 1 on a one-field struct.
        let bad_field = [
            0x00, 0xfb, 0x01, 0x01, 0xfb, 0x02, 0x01, 0x01, 0x1a, 0x0b,
        ];
        assert_eq!(
            check(&struct_mod(&bad_field, 0x01)),
            Err(ValidateError::UndefinedField)
        );
    }

    #[test]
    fn gc_struct_set_requires_a_mutable_field() {
        // struct.new_default $1 ; i32.const 5 ; struct.set $1 0
        let body = [
            0x00, 0xfb, 0x01, 0x01, 0x41, 0x05, 0xfb, 0x05, 0x01, 0x00, 0x0b,
        ];
        assert_eq!(check(&struct_mod(&body, 0x01)), Ok(()));
        assert_eq!(
            check(&struct_mod(&body, 0x00)),
            Err(ValidateError::ImmutableField)
        );
    }

    #[test]
    fn gc_packed_field_needs_a_sign_aware_get() {
        // A packed i8 field (storage 0x78) must be read with struct.get_s/_u, not struct.get.
        let plain = [
            0x00, 0xfb, 0x01, 0x01, 0xfb, 0x02, 0x01, 0x00, 0x1a, 0x0b,
        ];
        let packed = asm(&[
            (
                1,
                vec![0x02, 0x60, 0x00, 0x00, 0x5f, 0x01, 0x78, 0x01],
            ),
            (3, vec![0x01, 0x00]),
            (10, code1(&plain)),
        ]);
        assert_eq!(check(&packed), Err(ValidateError::TypeMismatch));

        // struct.get_s (0x03) on the same field is well-typed.
        let signed = [
            0x00, 0xfb, 0x01, 0x01, 0xfb, 0x03, 0x01, 0x00, 0x1a, 0x0b,
        ];
        let ok = asm(&[
            (
                1,
                vec![0x02, 0x60, 0x00, 0x00, 0x5f, 0x01, 0x78, 0x01],
            ),
            (3, vec![0x01, 0x00]),
            (10, code1(&signed)),
        ]);
        assert_eq!(check(&ok), Ok(()));
    }

    /// `() -> results` plus a tag of type `(i32) -> ()`.
    fn eh_mod(body: &[u8], results: &[u8]) -> Vec<u8> {
        let mut ty = vec![0x02u8, 0x60, 0x00];
        ty.push(results.len() as u8);
        ty.extend_from_slice(results);
        ty.extend_from_slice(&[0x60, 0x01, 0x7f, 0x00]);
        asm(&[
            (1, ty),
            (3, vec![0x01, 0x00]),
            (13, vec![0x01, 0x00, 0x01]),
            (10, code1(body)),
        ])
    }

    #[test]
    fn eh_try_table_typing() {
        // (block (result i32) (try_table (catch $e 1) (i32.const 42) (throw $e)))
        let ok = [
            0x00, 0x02, 0x7f, 0x1f, 0x7f, 0x01, 0x00, 0x00, 0x01, 0x41, 0x2a, 0x08, 0x00, 0x0b,
            0x0b, 0x0b,
        ];
        assert_eq!(check(&eh_mod(&ok, &[0x7f])), Ok(()));

        // A `catch_all` clause binds nothing, so its target label must carry no values —
        // here it targets a `(result i32)` block.
        let bad_all = [
            0x00, 0x02, 0x7f, 0x1f, 0x7f, 0x01, 0x02, 0x01, 0x41, 0x2a, 0x08, 0x00, 0x0b, 0x0b,
            0x0b,
        ];
        assert_eq!(
            check(&eh_mod(&bad_all, &[0x7f])),
            Err(ValidateError::TypeMismatch)
        );
    }

    #[test]
    fn eh_throw_checks_the_tag() {
        // throw $e with no i32 on the stack.
        let underflow = [0x00, 0x08, 0x00, 0x0b];
        assert_eq!(
            check(&eh_mod(&underflow, &[])),
            Err(ValidateError::StackUnderflow)
        );

        // A tag index out of range.
        let bad_tag = [0x00, 0x41, 0x00, 0x08, 0x09, 0x0b];
        assert_eq!(
            check(&eh_mod(&bad_tag, &[])),
            Err(ValidateError::UndefinedTag)
        );
    }

    #[test]
    fn eh_legacy_try_catch_typing() {
        // (try (result i32) (i32.const 3) (throw $e) (catch $e))
        let ok = [
            0x00, 0x06, 0x7f, 0x41, 0x03, 0x08, 0x00, 0x07, 0x00, 0x0b, 0x0b,
        ];
        assert_eq!(check(&eh_mod(&ok, &[0x7f])), Ok(()));

        // A bare `catch` whose enclosing opener is a plain block, not a `try`.
        let bare = [0x00, 0x02, 0x40, 0x07, 0x00, 0x0b, 0x0b];
        assert_eq!(
            check(&eh_mod(&bare, &[])),
            Err(ValidateError::MismatchedCatch)
        );
    }

    #[test]
    fn eh_delegate_is_rejected() {
        // `delegate` is refused outright — the interpreter cannot route it, and the frozen
        // oracle's validator rejects it, so text and binary paths agree.
        let body = [
            0x00, 0x02, 0x40, 0x06, 0x40, 0x41, 0x04, 0x08, 0x00, 0x18, 0x00, 0x0b, 0x0b,
        ];
        assert_eq!(
            check(&eh_mod(&body, &[])),
            Err(ValidateError::UnsupportedValidation)
        );
    }

    // ---- Proposal gating (T8) -------------------------------------------------------
    //
    // One vector per gateable proposal, each a module whose ONLY post-1.0 construct is
    // that proposal. Each is checked twice: it must validate with `Features::all()` (so
    // the vector is genuinely a valid module and the gate is what rejected it), and it
    // must be refused **naming that exact proposal** with only that one flag cleared.
    //
    // A vector that failed for some unrelated reason would pass a one-sided
    // "assert it errors" test while proving nothing — hence the positive half.

    /// `(feature, a module using exactly that feature)`.
    const GATE_VECTORS: &[(Feature, &str)] = &[
        (
            Feature::SignExtension,
            "(module (func (result i32) i32.const 1 i32.extend8_s))",
        ),
        (
            Feature::SaturatingFloatToInt,
            "(module (func (result i32) f32.const 1 i32.trunc_sat_f32_s))",
        ),
        (
            Feature::MultiValue,
            "(module (func (result i32 i32) i32.const 1 i32.const 2))",
        ),
        (
            Feature::ReferenceTypes,
            "(module (func (result externref) ref.null extern))",
        ),
        (
            Feature::BulkMemory,
            "(module (memory 1) (func i32.const 0 i32.const 0 i32.const 0 memory.fill))",
        ),
        (
            Feature::ExtendedConst,
            "(module (global i32 (i32.add (i32.const 1) (i32.const 2))))",
        ),
        (
            Feature::Simd,
            "(module (func (result v128) v128.const i32x4 0 0 0 0))",
        ),
        (
            Feature::RelaxedSimd,
            "(module (func (param v128 v128) (result v128) \
               local.get 0 local.get 1 i8x16.relaxed_swizzle))",
        ),
        (
            Feature::Threads,
            "(module (memory 1) (func (result i32) i32.const 0 i32.atomic.load))",
        ),
        (Feature::MultiMemory, "(module (memory 1) (memory 1))"),
        (Feature::Memory64, "(module (memory i64 1))"),
        (
            Feature::FunctionReferences,
            "(module (func (param funcref) local.get 0 ref.as_non_null drop))",
        ),
        (
            Feature::Gc,
            "(module (func (result i32) i32.const 1 ref.i31 i31.get_s))",
        ),
        (Feature::Exceptions, "(module (tag $e) (func throw $e))"),
    ];

    fn decode_wat(src: &str) -> Module {
        let bin = crate::wat::assemble(src.as_bytes())
            .unwrap_or_else(|e| panic!("assembling {src:?} failed: {e:?}"));
        decode(&bin).unwrap_or_else(|e| panic!("decoding {src:?} failed: {e:?}"))
    }

    // ---- `br_table` label typing (T9a, from `br_table.wast`) -------------------------
    //
    // §3.3.5.8 asks for ONE operand sequence that satisfies every target. Two things fall
    // out of that, and the validator had both backwards.

    #[test]
    fn br_table_targets_need_not_be_related_to_each_other() {
        // `meet-bottom`. After `unreachable` the operands are bottom, a subtype of every
        // type, so an `f32` target and an `f64` one are both satisfiable at once. Comparing
        // the targets pairwise instead of against the operands rejected this valid module.
        let md = decode_wat(
            r#"(module (func
                 (block (result f64)
                   (block (result f32) (unreachable) (br_table 0 1 1 (i32.const 1)))
                   (drop) (f64.const 0))
                 (drop)))"#,
        );
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn br_table_keeps_the_operand_type_across_targets() {
        // `meet-funcref`. Checking a target used to push the TARGET's types back onto the
        // stack instead of the operands that were popped, widening it — so a later target
        // narrower than an earlier one saw `(ref null func)` where `(ref null $t)` was
        // required and the module was refused.
        let md = decode_wat(
            r#"(module (type $t (func))
                 (func (param i32) (result (ref null func))
                   (block $l1 (result (ref null func))
                     (block $l2 (result (ref null $t))
                       (br_table $l1 $l1 $l2 (ref.null $t) (local.get 0))))))"#,
        );
        assert_eq!(validate(&md), Ok(()));
    }

    #[test]
    fn br_table_still_rejects_a_target_the_operands_do_not_fit() {
        // The other side of dropping the pairwise check: with real (non-bottom) operands,
        // each target is still checked against them, so a genuinely wrong one is refused.
        let md = decode_wat(
            r#"(module (func
                 (block (result f64)
                   (block (result f32) (f32.const 1) (br_table 0 1 (i32.const 1)))
                   (drop) (f64.const 0))
                 (drop)))"#,
        );
        assert_eq!(validate(&md), Err(ValidateError::TypeMismatch));
    }

    #[test]
    fn br_table_targets_must_agree_on_arity() {
        let md = decode_wat(
            r#"(module (func
                 (block (result f32)
                   (block (i32.const 1) (br_table 0 1 (i32.const 0)))
                   (f32.const 0))
                 (drop)))"#,
        );
        assert_eq!(validate(&md), Err(ValidateError::TypeMismatch));
    }

    #[test]
    fn every_gate_vector_is_valid_with_all_features_on() {
        for (f, src) in GATE_VECTORS {
            let md = decode_wat(src);
            assert_eq!(
                validate_with_features(&md, &Features::all()),
                Ok(()),
                "the {f} vector must be a VALID module, else its rejection proves nothing"
            );
        }
    }

    #[test]
    fn clearing_one_flag_rejects_exactly_that_proposal() {
        for (f, src) in GATE_VECTORS {
            let md = decode_wat(src);
            let mut fs = Features::all();
            fs.set(*f, false);
            assert_eq!(
                validate_with_features(&md, &fs),
                Err(ValidateError::FeatureDisabled(*f)),
                "with {f} off, {src:?} must be refused naming {f}"
            );
        }
    }

    #[test]
    fn an_unrelated_flag_does_not_reject() {
        // Turning off SIMD must not disturb the sign-extension vector, and vice versa —
        // the gate has to be specific, not a blanket "post-1.0 rejected".
        let md = decode_wat("(module (func (result i32) i32.const 1 i32.extend8_s))");
        let mut fs = Features::all();
        fs.simd = false;
        fs.relaxed_simd = false;
        assert_eq!(validate_with_features(&md, &fs), Ok(()));
    }

    #[test]
    fn plain_validate_still_accepts_every_proposal() {
        // `validate` == `validate_with_features(.., all())`, so nothing pre-T8 changed.
        for (_, src) in GATE_VECTORS {
            assert_eq!(validate(&decode_wat(src)), Ok(()));
        }
    }

    #[test]
    fn a_disabled_proposal_is_caught_through_a_type_not_just_an_opcode() {
        // The subtle half: SIMD off must also refuse a module that merely *declares* a
        // v128 — a local, a global, or a parameter — with no vector instruction anywhere.
        // Gating only the 0xFD opcodes would let all three through.
        let mut fs = Features::all();
        fs.simd = false;
        fs.relaxed_simd = false;
        for src in [
            "(module (func (local v128)))",
            "(module (global (mut v128) (v128.const i32x4 0 0 0 0)))",
            "(module (func (param v128)))",
            "(module (type $t (func (result v128))))",
        ] {
            assert_eq!(
                validate_with_features(&decode_wat(src), &fs),
                Err(ValidateError::FeatureDisabled(Feature::Simd)),
                "{src:?} declares a v128 and must be refused with SIMD off"
            );
        }
    }

    #[test]
    fn a_gc_type_definition_is_refused_with_gc_off() {
        // Same shape for GC: a struct type in the type section, never instantiated.
        let mut fs = Features::all();
        fs.gc = false;
        assert_eq!(
            validate_with_features(
                &decode_wat("(module (type $s (struct (field i32))))"),
                &fs
            ),
            Err(ValidateError::FeatureDisabled(Feature::Gc))
        );
    }

    #[test]
    fn mvp_accepts_a_webassembly_1_0_module_and_refuses_the_rest() {
        // The floor is usable: a plain 1.0 module still validates with everything off.
        let md = decode_wat(
            "(module (memory 1) (table 1 funcref) (global (mut i32) (i32.const 0))
               (func (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)
               (func (result i32) i32.const 0 i32.load))",
        );
        assert_eq!(validate_with_features(&md, &Features::mvp()), Ok(()));
    }

    /// Minimal unsigned-LEB128 encoder for the test byte-builders.
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
mod failure_location_tests {
    use super::*;
    use crate::module::decode;

    fn md(src: &str) -> crate::module::Module {
        decode(&crate::wat::assemble(src.as_bytes()).unwrap()).unwrap()
    }

    /// The index is in the **function index space** (imports included), which is the numbering the
    /// CLI, `ref.func` and the name section all use — not the position within the code section.
    #[test]
    fn a_body_failure_reports_the_function_index_including_imports() {
        let m = md(concat!(
            r#"(module (import "e" "f" (func)) (func) "#,
            r#"(func (result i32) (i64.const 1)))"#
        ));
        assert!(validate(&m).is_err());
        // 1 import + the empty function + the bad one -> index 2.
        assert_eq!(last_failure_func_index(), Some(2));
    }

    /// A module-level failure has no function to blame, and must say so rather than point at
    /// whatever the previous module happened to leave behind.
    #[test]
    fn a_module_level_failure_reports_no_location() {
        let bad_body = md(r#"(module (func (result i32) (i64.const 1)))"#);
        assert!(validate(&bad_body).is_err());
        assert_eq!(last_failure_func_index(), Some(0));

        // Now a failure that is not in any body: a start function with the wrong signature.
        let bad_start = md(r#"(module (func $s (param i32)) (start $s))"#);
        assert!(validate(&bad_start).is_err());
        assert_eq!(
            last_failure_func_index(),
            None,
            "a module-level failure must not inherit the previous module's location"
        );
    }

    /// And a success must not leave a location behind for the next caller to misread.
    #[test]
    fn success_clears_the_location() {
        let bad = md(r#"(module (func (result i32) (i64.const 1)))"#);
        assert!(validate(&bad).is_err());
        assert!(last_failure_func_index().is_some());

        let good = md(r#"(module (func (result i32) (i32.const 1)))"#);
        assert!(validate(&good).is_ok());
        assert_eq!(last_failure_func_index(), None);
    }

    /// ⚠️ **T9a#9's fixture is INVALID, and this pins why.** `if (result f64)` whose arms both push
    /// `i32` is ill-typed (§3.3.5); the spec suite says so at `if.wast`'s
    /// `type-then-value-num-vs-num`. The punch list recorded it as "our type-checker is wrong,
    /// because the oracle assembles **and runs** them" — but the oracle's *execution* path does not
    /// validate. Its **validator** reports `TypeMismatch` on this same module, exactly as wasmrt
    /// does. Kept as a test so nobody re-opens it.
    #[test]
    fn the_t9a9_fixture_construct_is_genuinely_ill_typed() {
        let m = md(concat!(
            r#"(module (func (param f64 f64 f64) (result i32) "#,
            r#"local.get 0 local.get 1 f64.ge "#,
            r#"if (result f64) local.get 0 local.get 2 f64.le else i32.const 0 end "#,
            r#"return))"#
        ));
        assert_eq!(validate(&m), Err(ValidateError::TypeMismatch));
    }
}

#[cfg(test)]
mod wasmtime_shaped_diagnostic_tests {
    use super::*;
    use crate::module::decode;

    fn md(src: &str) -> crate::module::Module {
        decode(&crate::wat::assemble(src.as_bytes()).unwrap()).unwrap()
    }

    /// 🔒 **Parity with wasmtime, checked against the real tool.** wasmtime 47.0.2 on this module:
    ///
    /// ```text
    /// Invalid input WebAssembly code at offset 33: type mismatch: expected i32, found i64
    /// ```
    ///
    /// The offset is byte-identical because both count from the **start of the module**, so the two
    /// tools' numbers are directly comparable on the same file — which is the whole point of
    /// matching rather than inventing our own origin.
    #[test]
    fn the_offset_and_types_match_wasmtime_47() {
        let m = md(r#"(module (func (export "f") (result i32) i64.const 1))"#);
        assert_eq!(validate(&m), Err(ValidateError::TypeMismatch));

        let site = last_failure_site();
        assert_eq!(site.offset, Some(33), "wasmtime reports offset 33 for this module");
        assert_eq!(site.expected, Some(V::I32));
        assert_eq!(site.found, Some(V::I64));
        assert_eq!(site.func_index, Some(0));
    }

    /// The second case checked against the real tool: wasmtime reports **offset 61**, and the
    /// failing body is the third function. One data point could be a coincidence of encoding.
    #[test]
    fn the_offset_tracks_across_functions() {
        let m = md(concat!(
            r#"(module (func (export "a") (result i32) (i32.const 1))"#,
            r#" (func (export "b") (result i32) (i32.const 2))"#,
            r#" (func (export "c") (param f64) (result i32) (i32.const 1) (drop) (local.get 0)))"#
        ));
        assert_eq!(validate(&m), Err(ValidateError::TypeMismatch));

        let site = last_failure_site();
        assert_eq!(site.offset, Some(61), "wasmtime reports offset 61 for this module");
        assert_eq!(site.func_index, Some(2));
        assert_eq!((site.expected, site.found), (Some(V::I32), Some(V::F64)));
    }

    /// A success must leave nothing behind — the whole record, not just the function index. A stale
    /// offset would make the *next* failure point at the wrong instruction, which is worse than
    /// having no offset at all.
    #[test]
    fn success_clears_the_whole_record() {
        let bad = md(r#"(module (func (result i32) (i64.const 1)))"#);
        assert!(validate(&bad).is_err());
        assert!(last_failure_site() != FailureSite::default());

        let good = md(r#"(module (func (result i32) (i32.const 1)))"#);
        assert!(validate(&good).is_ok());
        assert_eq!(last_failure_site(), FailureSite::default());
    }

    /// A non-type-mismatch failure records where, and honestly reports no types rather than
    /// inventing a pair — the CLI falls back to the plain error text in that case.
    #[test]
    fn a_non_type_failure_reports_no_types() {
        let m = md(r#"(module (func (result i32) (i32.const 1) (i32.const 2)))"#);
        assert!(validate(&m).is_err());
        let site = last_failure_site();
        assert_eq!((site.expected, site.found), (None, None));
        assert_eq!(site.func_index, Some(0));
    }
}
