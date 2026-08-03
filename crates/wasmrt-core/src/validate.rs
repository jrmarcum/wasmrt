//! Type validation of a decoded module (WebAssembly spec §3, Appendix "Validation
//! Algorithm": an abstract operand-value stack + a control-frame stack, with a bottom
//! `unknown` type for stack-polymorphic / unreachable code).
//!
//! Ported from wazmrt `src/validate.zig` (T4). **Scope this release (v0.5.0):** the core
//! language — control flow, calls, parametric/variable/reference ops, tables, bulk memory,
//! i31 refs, loads/stores/numeric — plus module-level checks (const-exprs, elements, data,
//! limits, tags, exports, start) and the `C.refs` (undeclared-function-reference) rule.
//!
//! **Deferred to the next slice (v0.5.x, verified against the spec suite at T6):** the
//! typing arms for SIMD, threads/atomics, GC struct/array objects + casts, and exception
//! handling. Those ops **reject loudly** ([`ValidateError::UnsupportedValidation`]) — never
//! silent-accept — so "the validator accepted it" stays a trustworthy promise.
//!
//! `validate` does not mutate the module; it decodes each body to IR and type-checks it.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::module::{Code, FuncType, Module};
use crate::opcode::{self, decode_body, HeapType, Imm, Instr, Op, RefType};
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
    /// A typing arm not yet ported (SIMD / atomics / GC objects / EH). Loud by design.
    UnsupportedValidation,
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
            other => write!(f, "invalid module: {other:?}"),
        }
    }
}

impl core::error::Error for ValidateError {}

/// A validation result.
pub type ValidateResult<T> = core::result::Result<T, ValidateError>;

/// Validate an entire module. Returns on the first error.
pub fn validate(module: &Module) -> ValidateResult<()> {
    if module.functions.len() != module.code.len() {
        return Err(ValidateError::CountMismatch);
    }

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
        validate_const_expr(module, init_expr, expected, self_index, Some(&mut refs))?;
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
            validate_const_expr(module, ex, elem.elem_type, n_imported_globals, Some(&mut refs))?;
        }
        if elem.mode == crate::module::ElementMode::Active {
            let ti = elem.table_index as usize;
            if ti >= module.tables.len() {
                return Err(ValidateError::UndefinedTable);
            }
            let tet = module.tables[ti].element;
            // Family match (nullability normalized away).
            if elem.elem_type.nullable() != tet.nullable() {
                return Err(ValidateError::TypeMismatch);
            }
            validate_const_expr(module, &elem.offset_expr, V::I32, all_globals, None)?;
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
        validate_const_expr(module, &seg.offset_expr, off_ty, all_globals, None)?;
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
    for (&type_index, code) in module.functions.iter().zip(&module.code) {
        let ft = module.func_sig(type_index).ok_or(ValidateError::UndefinedType)?;
        validate_function(module, &ft, code, Some(&refs))?;
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
                let heap =
                    opcode::read_heap_type(&mut r).map_err(|_| ValidateError::ConstantExpressionRequired)?;
                let vt = ref_type_val_type(
                    module,
                    RefType {
                        nullable: true,
                        heap,
                    },
                )?;
                push(&mut stack, vt)?;
            }
            0xd2 => {
                // ref.func x
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
            0x6a..=0x6c => {
                // i32 add/sub/mul (extended-const)
                let n = stack.len();
                if n < 2 || stack[n - 1] != V::I32 || stack[n - 2] != V::I32 {
                    return Err(ValidateError::TypeMismatch);
                }
                stack.pop();
            }
            0x7c..=0x7e => {
                // i64 add/sub/mul (extended-const)
                let n = stack.len();
                if n < 2 || stack[n - 1] != V::I64 || stack[n - 2] != V::I64 {
                    return Err(ValidateError::TypeMismatch);
                }
                stack.pop();
            }
            // v128.const (0xfd 0x0c) and the GC constant instructions (0xfb …) are
            // deferred to v0.5.x — reject as not-a-constant-expression for now.
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
        locals.resize(locals.len() + l.count as usize, l.ty);
    }

    let instrs = decode_body(&code.body)?;

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
        refs,
        locals: &locals,
        results: &ft.results,
        local_init,
        vals: Vec::new(),
        ctrls: Vec::new(),
    };
    // The whole body is an implicit block of type [] -> results.
    v.push_ctrl(FrameKind::Block, Vec::new(), ft.results.clone())?;
    for instr in &instrs {
        v.step(instr)?;
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
    refs: Option<&'a [bool]>,
    locals: &'a [V],
    results: &'a [V],
    local_init: Vec<bool>,
    vals: Vec<StackType>,
    ctrls: Vec<Frame>,
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
    fn table_elem_type(&self, i: u32) -> ValidateResult<V> {
        self.module
            .tables
            .get(i as usize)
            .map(|t| t.element)
            .ok_or(ValidateError::UndefinedTable)
    }

    /// (pop, push) types of a block signature (§5.3.6).
    fn block_sig(&self, bt: opcode::BlockType) -> ValidateResult<(Vec<V>, Vec<V>)> {
        match bt {
            opcode::BlockType::Empty => Ok((Vec::new(), Vec::new())),
            opcode::BlockType::Value(t) => Ok((Vec::new(), vec![t])),
            opcode::BlockType::TypeIndex(i) => {
                let ft = self.module.func_sig(i).ok_or(ValidateError::UndefinedType)?;
                Ok((ft.params, ft.results))
            }
        }
    }

    fn step(&mut self, instr: &Instr) -> ValidateResult<()> {
        if self.ctrls.is_empty() {
            return Err(ValidateError::ControlUnderflow); // code after the final `end`
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
                    if lt.len() != default_lt.len() {
                        return Err(ValidateError::TypeMismatch);
                    }
                    // Every target must be type-compatible with the default (both ways —
                    // rejects only genuinely incompatible pairs, safe in polymorphic code).
                    for (a, b) in lt.iter().zip(&default_lt) {
                        if !subtype_of(self.module, *a, *b) && !subtype_of(self.module, *b, *a) {
                            return Err(ValidateError::TypeMismatch);
                        }
                    }
                    self.pop_vals(&lt)?;
                    self.push_vals(&lt);
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
                if ft.results.as_slice() != self.results {
                    return Err(ValidateError::TypeMismatch);
                }
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
const STORE_I64: &[V] = &[V::I32, V::I64];
const STORE_F32: &[V] = &[V::I32, V::F32];
const STORE_F64: &[V] = &[V::I32, V::F64];

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

    #[test]
    fn validates_well_typed_add() {
        let bytes = m(&[
            0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x0b, 0x01, 0x09, 0x01, 0x01, 0x7f, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
            0x07, 0x07, 0x01, 0x03, b'a', b'd', b'd', 0x00, 0x00,
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

    #[test]
    fn rejects_function_code_count_mismatch() {
        // function section declares 1 func, but no code section.
        let bytes = m(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00]);
        let md = decode(&bytes).unwrap();
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
