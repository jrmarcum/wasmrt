//! A decoded WebAssembly module — pipeline stage 1 (decode).
//!
//! Ported from wazmrt `src/Module.zig` (T3). Validate the header, index the top-level
//! sections, and decode the type / import / function / tag / table / memory / global /
//! export / element / code / data sections. Every import and export is resolved to its
//! full [`Extern`] type. Validation, instantiation, and execution build on this type.
//!
//! **Ownership:** unlike wazmrt (which arena-owns everything), a `Module` holds owned
//! `Vec`/`String` fields, so it is independent of the input `bytes` and frees itself on
//! drop — there is no `deinit`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::opcode::HeapType;
use crate::reader::Reader;
use crate::types::{
    DecodeError, DecodeResult, ExternKind, RefHeap, SectionId, ValType, MAGIC, SUPPORTED_VERSION,
};

/// A top-level section, indexed by id and payload location (metadata only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub id: SectionId,
    pub offset: usize,
    pub size: usize,
}

/// A function signature from the type section (§5.3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// A struct/array field's storage type (GC, §5.3.6). Packed `i8`/`i16` store narrow and
/// widen on read; an unpacked field holds an ordinary value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageType {
    Val(ValType),
    I8,
    I16,
}

impl StorageType {
    /// The value type this field projects onto the operand stack (packed → i32).
    #[must_use]
    pub const fn unpacked(self) -> ValType {
        match self {
            StorageType::Val(v) => v,
            StorageType::I8 | StorageType::I16 => ValType::I32,
        }
    }

    #[must_use]
    pub const fn is_packed(self) -> bool {
        !matches!(self, StorageType::Val(_))
    }
}

/// A struct field / array element type (GC): storage type + mutability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldType {
    pub storage: StorageType,
    pub mutable: bool,
}

/// The composite-type kind of a type-section entry (GC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompKind {
    Func,
    Struct,
    Array,
}

/// A composite type from the type section (§5.3): a function signature, a struct (a
/// vector of fields), or an array (a single element field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompType {
    Func(FuncType),
    Struct(Vec<FieldType>),
    Array(FieldType),
}

impl CompType {
    #[must_use]
    pub const fn kind(&self) -> CompKind {
        match self {
            CompType::Func(_) => CompKind::Func,
            CompType::Struct(_) => CompKind::Struct,
            CompType::Array(_) => CompKind::Array,
        }
    }
}

/// Resizable-range limits shared by tables and memories (§5.3.7). `shared` (threads) and
/// `is64` (memory64) apply only to memories. `min`/`max` are PAGE counts (a 64-bit memory
/// may declare up to 2^48 pages, so they are `u64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub min: u64,
    pub max: Option<u64>,
    pub shared: bool,
    pub is64: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableType {
    pub element: ValType,
    pub limits: Limits,
    /// Raw constant-expression bytes (including the terminating `end`) every entry starts
    /// as — the function-references `0x40 0x00 tabletype expr` form. `None` is the plain
    /// form, whose entries start null.
    ///
    /// Only a *defined* table can carry one; an imported table takes its contents from the
    /// exporter.
    pub init: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryType {
    pub limits: Limits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalType {
    pub content: ValType,
    pub mutable: bool,
}

/// The resolved type of an import or export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extern {
    Func(FuncType),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
    /// An imported/exported exception tag (EH): a function type whose params are the
    /// exception's value types.
    Tag(FuncType),
}

impl Extern {
    #[must_use]
    pub const fn kind(&self) -> ExternKind {
        match self {
            Extern::Func(_) => ExternKind::Func,
            Extern::Table(_) => ExternKind::Table,
            Extern::Memory(_) => ExternKind::Memory,
            Extern::Global(_) => ExternKind::Global,
            Extern::Tag(_) => ExternKind::Tag,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub ty: Extern,
    /// For a **function** or **tag** import, the type **index** it was declared with; `None` for
    /// the other kinds.
    ///
    /// `ty` resolves that index to a `FuncType` structure, which is what the engine runs on — but a
    /// structure cannot answer *which type* this is, and for import matching that is the question.
    /// Two functions can both be `(func)` and still be different types, because rec-group membership
    /// is part of identity: `type-subtyping.wast` links `M10.f` (declared in a group whose sibling
    /// refers outward) against a `$f11` (in a group whose sibling refers inward) and must refuse.
    /// Comparing param/result lists cannot see that; comparing type indices can.
    pub func_type_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    /// Index into the module's combined space for its kind (§5.5.10).
    pub index: u32,
    pub ty: Extern,
}

/// A data segment (§5.5.14). Active segments initialize linear memory at a const-expr
/// offset; passive segments are copied by `memory.init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSegment {
    pub active: bool,
    pub mem_index: u32,
    /// Raw constant-expression bytes (including the terminating `end`); empty for passive.
    pub offset_expr: Vec<u8>,
    pub bytes: Vec<u8>,
}

/// The mode of an element segment (§5.5.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementMode {
    Active,
    Passive,
    Declarative,
}

/// An element segment (§5.5.12). Either a function-index list (`funcs`) or a vector of
/// const-expressions (`exprs`); exactly one is non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub mode: ElementMode,
    pub table_index: u32,
    pub offset_expr: Vec<u8>,
    pub funcs: Vec<u32>,
    pub exprs: Vec<Vec<u8>>,
    pub elem_type: ValType,
}

/// A run of consecutive locals of the same type in a code entry (§5.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub count: u32,
    pub ty: ValType,
}

/// A defined function's body from the code section (§5.5.13): its declared locals and its
/// instructions, decoded at decode time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub locals: Vec<Local>,
    /// The body decoded to instructions, produced **at decode time**.
    ///
    /// Two reasons it lives here rather than being decoded on demand. First, correctness of
    /// *stage*: a malformed instruction encoding is a decode error by the spec, and while bodies
    /// were decoded lazily the decoder accepted modules the validator then had to reject —
    /// `assert_malformed` cases surfacing as validation failures. Second, it is less work: the
    /// validator and every instantiation each used to decode the same bytes again.
    ///
    /// This **replaced** a `body: Vec<u8>` field rather than joining it. Keeping both meant a
    /// second copy of every function body in every module, which is measurable on cold start —
    /// cold start being mostly decode — and nothing read the bytes once the IR existed.
    /// `body_offset` is what the raw form was actually needed for.
    pub ir: Vec<crate::opcode::Instr>,
    /// Absolute byte offset of the body within the original module binary (for truthful
    /// trap backtraces).
    pub body_offset: u32,
}

impl Code {
    /// Total number of declared locals (excludes parameters).
    #[must_use]
    pub fn local_count(&self) -> u64 {
        self.locals.iter().map(|l| u64::from(l.count)).sum()
    }
}

/// A decoded WebAssembly module. All fields are owned; a `Module` outlives its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub version: u32,
    pub sections: Vec<Section>,
    /// The type index space (§5.3): func / struct / array composite types, in declaration
    /// order (rec groups flattened into consecutive indices).
    pub comp_types: Vec<CompType>,
    /// Declared supertype of each type index (GC sub types), or `None`. Read by
    /// [`Module::is_subtype`].
    pub supertypes: Vec<Option<u32>>,
    /// `(start, len)` of each rec group, in type-index order. A type written without `(rec …)` is its
    /// own singleton group, which is what the spec says it is.
    ///
    /// Kept because the group — not the individual type — is the unit of identity, so anything that
    /// re-derives identity later (the store's cross-module type registry) needs the boundaries, and
    /// they cannot be recovered from `type_canon` alone.
    pub rec_groups: Vec<(u32, u32)>,
    /// **Canonical type identity**, parallel to `comp_types`: `type_canon[t]` is the *lowest* type
    /// index structurally equal to `t`, so two types are the same type iff their entries match.
    ///
    /// The spec decides type identity **structurally**, on canonical rec groups (§3.1.4) — two rec
    /// groups spelling out the same shape are one type. wasmrt otherwise compares concrete types by
    /// **index**, which is a different relation, and the gap showed up four ways at once: valid
    /// modules rejected, `ref.test` answering wrongly, `assert_trap` cases returning a result, and
    /// invalid modules accepted.
    ///
    /// Empty for a hand-built `Module` that never went through `decode`; [`Module::canon_id`] then
    /// falls back to the identity, which is exactly the old index-comparison behaviour.
    pub type_canon: Vec<u32>,
    /// Whether each type is **final** — closed to further subtyping. Parallel to `comp_types`.
    ///
    /// Final is the *default*: only the `0x50` (`sub`) wrapper opens a type. `0x4f` is `sub final`
    /// and a bare composite type is shorthand for `sub final ϵ`. Kept as its own vector rather than
    /// folded into `supertypes` because the two are independent — a type can be open with no
    /// supertype, or final with one.
    pub type_finals: Vec<bool>,
    /// Type index of each *defined* function, in order.
    pub functions: Vec<u32>,
    /// Type index of each *defined* exception tag (§5.5.14, EH).
    pub tags: Vec<u32>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    /// Body of each defined function, positionally matching `functions`.
    pub code: Vec<Code>,
    /// The global index space (imported globals first, then defined).
    pub globals: Vec<GlobalType>,
    /// Init const-expr bytes for each *defined* global (the tail of `globals`).
    pub global_inits: Vec<Vec<u8>>,
    /// The memory index space (imported memories first, then defined).
    pub memories: Vec<MemoryType>,
    /// The table index space (imported tables first, then defined).
    pub tables: Vec<TableType>,
    pub data: Vec<DataSegment>,
    pub elements: Vec<Element>,
    /// The start function's index (§5.5.11).
    pub start: Option<u32>,
    /// Raw `vec(nameassoc)` payload of the name section's function-name subsection
    /// (§7.4.2), or `None`. Read via [`Module::func_name`] (consulted only on a trap).
    pub func_names: Option<Vec<u8>>,
}

impl Module {
    /// Return the first section with `id`, or `None`.
    #[must_use]
    pub fn section(&self, id: SectionId) -> Option<Section> {
        self.sections.iter().copied().find(|s| s.id == id)
    }

    /// Number of imported functions (they occupy the low function indices).
    #[must_use]
    pub fn imported_func_count(&self) -> u32 {
        self.imports.iter().filter(|i| matches!(i.ty, Extern::Func(_))).count() as u32
    }

    /// Number of imported exception tags.
    #[must_use]
    pub fn imported_tag_count(&self) -> u32 {
        self.imports.iter().filter(|i| matches!(i.ty, Extern::Tag(_))).count() as u32
    }

    /// Number of imported tables.
    #[must_use]
    pub fn imported_table_count(&self) -> u32 {
        self.imports.iter().filter(|i| matches!(i.ty, Extern::Table(_))).count() as u32
    }

    /// Number of imported memories.
    #[must_use]
    pub fn imported_memory_count(&self) -> u32 {
        self.imports.iter().filter(|i| matches!(i.ty, Extern::Memory(_))).count() as u32
    }

    /// Resolve a function index (imports first, then defined) to its signature.
    #[must_use]
    pub fn func_type(&self, index: u32) -> Option<FuncType> {
        let mut i: u32 = 0;
        for imp in &self.imports {
            if let Extern::Func(ft) = &imp.ty {
                if i == index {
                    return Some(ft.clone());
                }
                i += 1;
            }
        }
        let defined = (index - i) as usize;
        let ti = *self.functions.get(defined)?;
        self.func_sig(ti)
    }

    /// The type index of a function (imports first, then defined), or `None` for an
    /// imported function.
    #[must_use]
    pub fn func_type_index(&self, func_index: u32) -> Option<u32> {
        let imported = self.imported_func_count();
        if func_index < imported {
            return None;
        }
        let defined = (func_index - imported) as usize;
        self.functions.get(defined).copied()
    }

    /// The function signature at type index `ti`, or `None` if out of range or a
    /// non-function composite type.
    #[must_use]
    pub fn func_sig(&self, ti: u32) -> Option<FuncType> {
        match self.comp_types.get(ti as usize)? {
            CompType::Func(f) => Some(f.clone()),
            _ => None,
        }
    }

    /// The **type index** an exception tag names (imports first, then defined), which is the tag's
    /// identity for §4.5.9 matching — [`Self::tag_type`] gives only its shape.
    #[must_use]
    pub fn tag_type_index(&self, tag_index: u32) -> Option<u32> {
        let mut i: u32 = 0;
        for imp in &self.imports {
            if matches!(imp.ty, Extern::Tag(_)) {
                if i == tag_index {
                    return imp.func_type_index;
                }
                i += 1;
            }
        }
        self.tags.get((tag_index - i) as usize).copied()
    }

    /// The function type an exception `tag` names (imports first, then defined).
    #[must_use]
    pub fn tag_type(&self, tag_index: u32) -> Option<FuncType> {
        let mut i: u32 = 0;
        for imp in &self.imports {
            if let Extern::Tag(ft) = &imp.ty {
                if i == tag_index {
                    return Some(ft.clone());
                }
                i += 1;
            }
        }
        let defined = (tag_index - i) as usize;
        let ti = *self.tags.get(defined)?;
        self.func_sig(ti)
    }

    /// The struct field vector at type index `ti`, or `None` if not a struct.
    #[must_use]
    pub fn struct_fields(&self, ti: u32) -> Option<&[FieldType]> {
        match self.comp_types.get(ti as usize)? {
            CompType::Struct(fs) => Some(fs),
            _ => None,
        }
    }

    /// The array element field at type index `ti`, or `None` if not an array.
    #[must_use]
    pub fn array_field(&self, ti: u32) -> Option<FieldType> {
        match self.comp_types.get(ti as usize)? {
            CompType::Array(f) => Some(*f),
            _ => None,
        }
    }

    /// Resolve a GC heap type to a reference-hierarchy head, mapping a concrete type index
    /// to its composite family. Errors if a concrete index is out of range.
    pub fn ref_head(&self, ht: HeapType) -> DecodeResult<RefHeap> {
        Ok(match ht {
            HeapType::Func => RefHeap::Func,
            HeapType::Extern => RefHeap::Extern,
            HeapType::NoFunc => RefHeap::NoFunc,
            HeapType::NoExtern => RefHeap::NoExtern,
            HeapType::NoExn => RefHeap::NoExn,
            HeapType::Any => RefHeap::Any,
            HeapType::Eq => RefHeap::Eq,
            HeapType::I31 => RefHeap::I31,
            HeapType::Struct => RefHeap::Struct,
            HeapType::Array => RefHeap::Array,
            HeapType::None => RefHeap::None,
            HeapType::Exn => RefHeap::Exn,
            HeapType::Concrete(ti) => {
                let ct = self
                    .comp_types
                    .get(ti as usize)
                    .ok_or(DecodeError::IndexOutOfRange)?;
                match ct.kind() {
                    CompKind::Func => RefHeap::Func,
                    CompKind::Struct => RefHeap::Struct,
                    CompKind::Array => RefHeap::Array,
                }
            }
        })
    }

    /// Is type index `a` a (reflexive/transitive) subtype of `b`, walking the declared GC
    /// supertype chain?
    #[must_use]
    pub fn is_subtype(&self, a: u32, b: u32) -> bool {
        // Compared CANONICALLY, not by index: `a` may be a different index naming the same type as
        // `b`, or its supertype chain may pass through one. Every subtype question in the engine —
        // the validator's `subtype_of`, the declared-supertype check, and `ref.test`/`ref.cast` at
        // run time — comes through here, so this is where structural identity takes effect.
        let cb = self.canon_id(b);
        let mut cur = Some(a);
        while let Some(c) = cur {
            if self.canon_id(c) == cb {
                return true;
            }
            // Terminates because a declared supertype is always a strictly lower index (enforced at
            // decode), so the walk is finite even on a hand-built module.
            cur = self.supertypes.get(c as usize).copied().flatten();
        }
        false
    }

    /// The canonical identity of type `t` — the lowest type index structurally equal to it.
    ///
    /// Falls back to `t` itself when there is no canonical table (a `Module` built by hand rather
    /// than decoded), which reduces to comparing indices: the previous behaviour, not a panic.
    #[must_use]
    pub fn canon_id(&self, t: u32) -> u32 {
        self.type_canon.get(t as usize).copied().unwrap_or(t)
    }

    /// Are types `a` and `b` **the same type** (§3.1.4 structural identity)?
    #[must_use]
    pub fn types_equal(&self, a: u32, b: u32) -> bool {
        self.canon_id(a) == self.canon_id(b)
    }

    /// Are two value types the same type, deciding **concrete** references canonically?
    ///
    /// `==` on the packed bits is not the same question: a concrete reference packs a module-local
    /// type index, so two spellings of one type compare unequal. Nullability is part of the type and
    /// must still match.
    #[must_use]
    pub fn val_types_equal(&self, a: ValType, b: ValType) -> bool {
        a == b
            || (a.is_concrete()
                && b.is_concrete()
                && a.is_non_null_ref() == b.is_non_null_ref()
                && self.types_equal(a.concrete_index(), b.concrete_index()))
    }

    /// Do two function signatures denote the same type, concrete references compared canonically?
    ///
    /// The slice equality is tried **first** because it is what almost every call sees, and it is a
    /// single memcmp; the per-element canonical walk runs only when the bits actually differ. That
    /// keeps `call_indirect`'s check off the canonical path in the common case.
    #[must_use]
    pub fn func_types_equal(&self, a: &FuncType, b: &FuncType) -> bool {
        if a.params == b.params && a.results == b.results {
            return true;
        }
        a.params.len() == b.params.len()
            && a.results.len() == b.results.len()
            && a.params
                .iter()
                .zip(&b.params)
                .all(|(&x, &y)| self.val_types_equal(x, y))
            && a.results
                .iter()
                .zip(&b.results)
                .all(|(&x, &y)| self.val_types_equal(x, y))
    }

    /// The name recorded for function `index` in the name section, if any. Scans linearly;
    /// a malformed entry just ends the scan (a bad name section is never a decode error).
    #[must_use]
    pub fn func_name(&self, index: u32) -> Option<&[u8]> {
        let bytes = self.func_names.as_deref()?;
        let mut r = Reader::new(bytes);
        let count = r.read_var_u32().ok()?;
        for _ in 0..count {
            let idx = r.read_var_u32().ok()?;
            let len = r.read_var_u32().ok()? as usize;
            let name = r.read_bytes(len).ok()?;
            if idx == index {
                return Some(name);
            }
            if idx > index {
                return None; // the vec is sorted by index
            }
        }
        None
    }
}

/// Working state threaded through the section decoders, accumulating the per-kind index
/// spaces (imported entries first, then defined) needed to resolve export indices.
#[derive(Default)]
struct Decoder {
    comp_types: Vec<CompType>,
    supertypes: Vec<Option<u32>>,
    type_finals: Vec<bool>,
    type_canon: Vec<u32>,
    rec_groups: Vec<(u32, u32)>,
    /// Composite kind of each type index, pre-scanned before bodies are decoded so a
    /// `(ref $t)` value type can collapse to the right family even for a forward reference.
    type_kinds: Vec<CompKind>,
    func_space: Vec<FuncType>,
    table_space: Vec<TableType>,
    mem_space: Vec<MemoryType>,
    global_space: Vec<GlobalType>,
    tag_space: Vec<FuncType>,
    global_init_space: Vec<Vec<u8>>,
}

/// Decode a WebAssembly binary into an owned [`Module`]. `bytes` may be freed afterward.
pub fn decode(bytes: &[u8]) -> DecodeResult<Module> {
    let mut r = Reader::new(bytes);
    if r.read_bytes(4)? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = r.read_u32_le()?;
    if version != SUPPORTED_VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }

    let mut d = Decoder::default();
    let mut sections: Vec<Section> = Vec::new();
    let mut functions: Vec<u32> = Vec::new();
    let mut tags: Vec<u32> = Vec::new();
    let mut imports: Vec<Import> = Vec::new();
    let mut exports: Vec<Export> = Vec::new();
    let mut code: Vec<Code> = Vec::new();
    let mut data: Vec<DataSegment> = Vec::new();
    let mut elements: Vec<Element> = Vec::new();
    let mut start: Option<u32> = None;
    let mut data_count: Option<u32> = None;
    let mut func_names: Option<Vec<u8>> = None;

    // Order index of the last non-custom section seen, to enforce §5.5.2.
    let mut last_order: Option<u8> = None;

    while !r.at_end() {
        let raw_id = r.read_byte()?;
        if raw_id > SectionId::MAX {
            return Err(DecodeError::InvalidSectionId);
        }
        let id = SectionId::from_u8(raw_id).ok_or(DecodeError::InvalidSectionId)?;
        // Sections appear at most once, in the fixed order (§5.5.2); custom sections are exempt.
        // Strictly greater, not `>=`: a repeated section is as malformed as a misordered one, and
        // without this the second occurrence silently *replaced* the first — a repeated function
        // section changed the module's function count and only surfaced later as a count mismatch.
        if let Some(ord) = id.order() {
            if last_order.is_some_and(|prev| ord <= prev) {
                return Err(DecodeError::SectionOrder);
            }
            last_order = Some(ord);
        }
        let size = r.read_var_u32()? as usize;
        let offset = r.pos();
        let payload = r.read_bytes(size)?;
        sections.push(Section { id, offset, size });

        let mut sub = Reader::new(payload);
        match id {
            SectionId::Custom => {
                let nlen = sub.read_var_u32()? as usize;
                let cname = sub.read_bytes(nlen)?;
                // A custom section's id is a name (§5.2.4), so it must be UTF-8.
                if core::str::from_utf8(cname).is_err() {
                    return Err(DecodeError::InvalidUtf8);
                }
                if cname == b"name" {
                    func_names = find_func_name_subsection(&mut sub);
                }
            }
            SectionId::Type => decode_type_section(&mut d, &mut sub)?,
            SectionId::Import => imports = decode_import_section(&mut d, &mut sub)?,
            SectionId::Function => functions = decode_function_section(&mut d, &mut sub)?,
            SectionId::Tag => tags = decode_tag_section(&mut d, &mut sub)?,
            SectionId::Table => decode_table_section(&mut d, &mut sub)?,
            SectionId::Memory => decode_memory_section(&mut d, &mut sub)?,
            SectionId::Global => decode_global_section(&mut d, &mut sub)?,
            SectionId::Export => exports = decode_export_section(&d, &mut sub)?,
            SectionId::Element => elements = decode_element_section(&d, &mut sub)?,
            SectionId::Code => code = decode_code_section(&d, &mut sub, offset)?,
            SectionId::Data => data = decode_data_section(&mut sub)?,
            SectionId::DataCount => data_count = Some(sub.read_var_u32()?),
            SectionId::Start => start = Some(sub.read_var_u32()?),
        }

        // The section's contents must occupy exactly the size it declared. Custom sections are
        // exempt — everything after the name is arbitrary payload by definition, and the `name`
        // subsection walk deliberately stops early.
        //
        // Leftover bytes are not a cosmetic disagreement: the outer reader has already skipped
        // `size` bytes, so a section that under-reads means the producer and the decoder disagree
        // about the section's contents while still agreeing where the next one starts. Whatever
        // was in the gap is simply not in the module we built.
        if id != SectionId::Custom && !sub.at_end() {
            return Err(DecodeError::SectionSizeMismatch);
        }
    }

    // Constant expressions are stored as raw bytes and read by the validator and the interpreter,
    // each with its own little reader — so a malformed *encoding* inside one (an over-long LEB, a
    // truncated immediate) used to surface as a validation error. Structurally checking them here
    // puts that where it belongs. Unlike function bodies the result is discarded rather than
    // stored: a const-expr is a handful of bytes and both consumers want the raw form, so decoding
    // it twice costs nothing worth the data-model change.
    for expr in module_const_exprs(&d, &elements, &data) {
        crate::opcode::decode_body(expr)?;
    }

    // The function and code sections must declare the same number of functions (§5.5.13). This is
    // a *decode*-stage check because it is a disagreement between two sections' structure, not a
    // typing fact — the validator also caught it, one stage too late, and `assert_malformed` is
    // the assertion the spec suite uses. Both empty is fine: a module may have neither section.
    if functions.len() != code.len() {
        return Err(DecodeError::FuncCodeCountMismatch);
    }

    // If present, the data-count section must equal the data-segment count (§5.5.16).
    if let Some(dc) = data_count {
        if dc as usize != data.len() {
            return Err(DecodeError::DataCountMismatch);
        }
    } else if code
        .iter()
        .flat_map(|c| c.ir.iter())
        .any(|i| matches!(i.op, crate::opcode::Op::MemoryInit | crate::opcode::Op::DataDrop))
    {
        // …and when ABSENT it is required, if any body references a data segment (bulk-memory).
        // The count is what lets `memory.init`'s segment index be checked without having read the
        // data section, so its absence is a decode-stage failure rather than a validation one.
        // Only reachable now that bodies are decoded here — the check needs the instructions.
        return Err(DecodeError::DataCountRequired);
    }

    Ok(Module {
        version,
        sections,
        comp_types: d.comp_types,
        supertypes: d.supertypes,
        type_finals: d.type_finals,
        type_canon: d.type_canon,
        rec_groups: d.rec_groups,
        functions,
        tags,
        imports,
        exports,
        code,
        globals: d.global_space,
        global_inits: d.global_init_space,
        memories: d.mem_space,
        tables: d.table_space,
        data,
        elements,
        start,
        func_names,
    })
}

/// Find the function-name subsection (id 1) inside a `name` custom section and return an
/// owned copy of its `vec(nameassoc)` payload. The name section is a convention: every
/// error degrades to "no names".
fn find_func_name_subsection(sub: &mut Reader) -> Option<Vec<u8>> {
    while !sub.at_end() {
        let kind = sub.read_byte().ok()?;
        let size = sub.read_var_u32().ok()? as usize;
        let payload = sub.read_bytes(size).ok()?;
        if kind == 1 {
            return Some(payload.to_vec());
        }
        if kind > 1 {
            break; // subsections are ordered; 2+ means we passed it
        }
    }
    None
}

// --- Low-level readers -------------------------------------------------------

/// Read one value type. Numeric types are themselves; abstract reference shorthands map to
/// their family head, and `(ref null? ht)` (0x63/0x64) resolves a concrete `$t` to its
/// family via the pre-scanned `kinds`.
fn read_val_type(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<ValType> {
    let b = r.read_byte()?;
    Ok(match b {
        0x7f => ValType::I32,
        0x7e => ValType::I64,
        0x7d => ValType::F32,
        0x7c => ValType::F64,
        0x7b => ValType::V128,
        0x70 => ValType::FUNCREF,
        0x6f => ValType::EXTERNREF,
        // The three hierarchy BOTTOMS. ⚠️ These aliased onto their tops — `nullfuncref` decoded as
        // `funcref` — which only null inhabits either way, but they are distinct types and
        // `ref.test (ref null nofunc)` on a real funcref must answer 0.
        0x73 => ValType::NULLFUNCREF,
        0x72 => ValType::NULLEXTERNREF,
        0x6e => ValType::ANYREF,
        0x6d => ValType::EQREF,
        0x6c => ValType::I31REF,
        0x6b => ValType::STRUCTREF,
        0x6a => ValType::ARRAYREF,
        0x71 => ValType::NULLREF, // none
        0x69 => ValType::EXNREF,
        0x74 => ValType::NULLEXNREF,
        // ⚠️ `0x57`–`0x68` are `ValType`'s INTERNAL tags for the non-null abstract references and
        // were accepted here as if they were value-type bytes. They are not: §5.3.5 spells
        // `(ref ht)` as `0x64 ht`, and nothing else. Accepting them let wasmrt read a binary no
        // other engine can — the mirror of the assembler emitting one (see `emit_val_type`), and
        // the pair is what kept the round trip green.
        0x63 => read_heap_type_ref(r, true, kinds)?, // (ref null ht)
        0x64 => read_heap_type_ref(r, false, kinds)?, // (ref ht) — non-nullable
        _ => return Err(DecodeError::BadValType),
    })
}

/// Map a `heaptype` (following a `0x63`/`0x64` ref prefix) to a reference value type. A
/// non-negative `s33` is a concrete type index collapsing to its family head; negative
/// encodings are the abstract heap types.
fn read_heap_type_ref(r: &mut Reader, nullable: bool, kinds: &[CompKind]) -> DecodeResult<ValType> {
    let ht = r.read_var_s33()?;
    if ht >= 0 {
        if ht > u32::MAX as i64 {
            return Err(DecodeError::IndexOutOfRange);
        }
        let ti = ht as u32;
        let kind = kinds
            .get(ti as usize)
            .ok_or(DecodeError::IndexOutOfRange)?;
        let head = match kind {
            CompKind::Func => RefHeap::Func,
            CompKind::Struct => RefHeap::Struct,
            CompKind::Array => RefHeap::Array,
        };
        return Ok(ValType::concrete_ref(nullable, head, ti));
    }
    let (n, nn) = match ht {
        -0x10 => (ValType::FUNCREF, ValType::FUNCREF_NN),
        -0x11 => (ValType::EXTERNREF, ValType::EXTERNREF_NN),
        -0x0d => (ValType::NULLFUNCREF, ValType::NULLFUNCREF_NN),
        -0x0e => (ValType::NULLEXTERNREF, ValType::NULLEXTERNREF_NN),
        -0x0c => (ValType::NULLEXNREF, ValType::NULLEXNREF_NN),
        -0x12 => (ValType::ANYREF, ValType::ANYREF_NN),
        -0x13 => (ValType::EQREF, ValType::EQREF_NN),
        -0x14 => (ValType::I31REF, ValType::I31REF_NN),
        -0x15 => (ValType::STRUCTREF, ValType::STRUCTREF_NN),
        -0x16 => (ValType::ARRAYREF, ValType::ARRAYREF_NN),
        -0x0f => (ValType::NULLREF, ValType::NULLREF_NN),
        -0x17 => (ValType::EXNREF, ValType::EXNREF_NN),
        _ => return Err(DecodeError::BadValType),
    };
    Ok(if nullable { n } else { nn })
}

fn read_val_types(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<Vec<ValType>> {
    let n = r.read_vec_len()?;
    let mut vts = Vec::with_capacity(n as usize);
    for _ in 0..n {
        vts.push(read_val_type(r, kinds)?);
    }
    Ok(vts)
}

/// Copy a length-prefixed name (§5.2.4) into an owned `String`, rejecting non-UTF-8.
fn read_name(r: &mut Reader) -> DecodeResult<String> {
    let n = r.read_var_u32()? as usize;
    let src = r.read_bytes(n)?;
    match core::str::from_utf8(src) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => Err(DecodeError::InvalidUtf8),
    }
}

fn read_limits(r: &mut Reader) -> DecodeResult<Limits> {
    let flag = r.read_byte()?;
    // bit 0 = has max, bit 1 = shared (threads), bit 2 = i64 index (memory64).
    if flag > 0x07 {
        return Err(DecodeError::MalformedFlag);
    }
    let is64 = flag & 0x04 != 0;
    let min: u64 = if is64 {
        r.read_var_u64()?
    } else {
        u64::from(r.read_var_u32()?)
    };
    let max: Option<u64> = if flag & 0x01 != 0 {
        Some(if is64 {
            r.read_var_u64()?
        } else {
            u64::from(r.read_var_u32()?)
        })
    } else {
        None
    };
    Ok(Limits {
        min,
        max,
        shared: flag & 0x02 != 0,
        is64,
    })
}

fn read_table_type(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<TableType> {
    let element = read_val_type(r, kinds)?;
    let limits = read_limits(r)?;
    // A table may be 64-bit (the table64 half of memory64, in scope since T13) but never
    // shared — there is no `shared` table type in any proposal wasmrt targets.
    if limits.shared {
        return Err(DecodeError::MalformedFlag);
    }
    Ok(TableType {
        element,
        limits,
        init: None,
    })
}

fn read_global_type(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<GlobalType> {
    let content = read_val_type(r, kinds)?;
    let mutb = r.read_byte()?;
    if mutb > 0x01 {
        return Err(DecodeError::MalformedFlag);
    }
    Ok(GlobalType {
        content,
        mutable: mutb != 0,
    })
}

fn read_field_type(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<FieldType> {
    let storage = read_storage_type(r, kinds)?;
    let mutb = r.read_byte()?;
    if mutb > 0x01 {
        return Err(DecodeError::MalformedFlag);
    }
    Ok(FieldType {
        storage,
        mutable: mutb != 0,
    })
}

/// Read a storage type: a packed `i8` (0x78) / `i16` (0x77), else a value type.
fn read_storage_type(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<StorageType> {
    match r.peek_byte()? {
        0x78 => {
            r.read_byte()?;
            Ok(StorageType::I8)
        }
        0x77 => {
            r.read_byte()?;
            Ok(StorageType::I16)
        }
        _ => Ok(StorageType::Val(read_val_type(r, kinds)?)),
    }
}

/// Skip a constant init expression (§5.4.9): a short instruction sequence terminated by
/// `end` (0x0B), handling const-expr opcodes so an operand byte is never mistaken for the
/// terminator.
fn skip_const_expr(r: &mut Reader) -> DecodeResult<()> {
    loop {
        match r.read_byte()? {
            0x0b => return Ok(()), // end
            0x41 | 0x23 | 0xd2 => r.skip_leb(5)?, // i32.const / global.get / ref.func
            0x42 => r.skip_leb(10)?,              // i64.const
            0x43 => {
                r.read_bytes(4)?; // f32.const
            }
            0x44 => {
                r.read_bytes(8)?; // f64.const
            }
            0xd0 => {
                r.read_var_s33()?; // ref.null (heaptype s33)
            }
            0xfd => {
                // SIMD prefix — only `v128.const` is a constant instruction.
                if r.read_var_u32()? == 0x0c {
                    r.read_bytes(16)?;
                }
            }
            0xfb => {
                // GC prefix — the constant GC instructions carry immediates to skip.
                match r.read_var_u32()? {
                    0x00 | 0x01 | 0x06 | 0x07 => {
                        r.read_var_u32()?; // struct.new* / array.new* : type index
                    }
                    0x08 => {
                        r.read_var_u32()?; // array.new_fixed : type index + count
                        r.read_var_u32()?;
                    }
                    _ => {}
                }
            }
            _ => {} // other zero-operand ops (extended-const arithmetic, etc.)
        }
    }
}

// --- Section decoders --------------------------------------------------------

/// Decode the type section (§5.5.4, GC §5.3): a vector of *rec types*. Runs a cheap kind
/// pre-scan first so a `(ref $t)` inside a field can collapse to the right family even for
/// a forward reference in the same rec group.
fn decode_type_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<()> {
    let mut scan = r.clone(); // Reader is a value cursor — clone for the pre-scan pass.
    d.type_kinds = prescan_type_kinds(&mut scan)?;

    let mut comp: Vec<CompType> = Vec::new();
    let mut supers: Vec<Option<u32>> = Vec::new();
    let mut finals: Vec<bool> = Vec::new();
    // (start, len) of each rec group. A type written without `(rec …)` is its own singleton group,
    // which is what the spec says it is — so this needs no special case downstream.
    let mut groups: Vec<(u32, u32)> = Vec::new();
    let mut nrec = r.read_var_u32()?;
    while nrec > 0 {
        nrec -= 1;
        let start = comp.len() as u32;
        if r.peek_byte()? == 0x4e {
            r.read_byte()?; // rec group
            let mut k = r.read_var_u32()?;
            while k > 0 {
                k -= 1;
                decode_sub_type(&d.type_kinds, r, &mut comp, &mut supers, &mut finals)?;
            }
        } else {
            decode_sub_type(&d.type_kinds, r, &mut comp, &mut supers, &mut finals)?;
        }
        groups.push((start, comp.len() as u32 - start));
    }
    d.type_canon = canonicalize(&comp, &supers, &finals, &groups);
    d.rec_groups = groups;
    d.comp_types = comp;
    d.supertypes = supers;
    d.type_finals = finals;
    Ok(())
}

/// Assign each type its **canonical identity**: the lowest type index structurally equal to it.
///
/// Rec groups are the unit of identity (§3.1.4), so each group is reduced to a structural key in
/// which a reference to a *member of the same group* becomes its position — making the group's shape
/// independent of where it landed in the index space — and a reference *outside* becomes the target's
/// already-assigned canonical id. Groups are keyed in order, so an outside reference is always to an
/// earlier group whose canonical id is known. The one exception is a reference forward out of the
/// group, which is invalid; it is keyed by a distinct sentinel so this stays **total**:
/// canonicalisation must never fail or panic on hostile input, and the bad index is the validator's
/// to report.
fn canonicalize(
    comp: &[CompType],
    supers: &[Option<u32>],
    finals: &[bool],
    groups: &[(u32, u32)],
) -> Vec<u32> {
    // A `BTreeMap`, not a linear scan over previously-seen keys: the number of rec groups is
    // attacker-controlled, and a scan would be O(groups²) — a module of 100k singleton groups would
    // be a denial of service on the decoder. `alloc`'s BTreeMap needs no dependency and no hasher.
    let mut seen: alloc::collections::BTreeMap<Vec<u8>, u32> = alloc::collections::BTreeMap::new();
    let mut canon: Vec<u32> = Vec::with_capacity(comp.len());
    for &(start, len) in groups {
        // `canon` is filled in index order, so its length is exactly `start` here — which is what
        // makes "outside the group" and "not yet canonicalised" the same test in `push_type_ref`.
        let key = rec_group_key(comp, supers, finals, &canon, start, len);
        let first = *seen.entry(key).or_insert(start);
        for i in 0..len {
            canon.push(first + i);
        }
    }
    canon
}

/// The structural key of one rec group, keyed against **arbitrary already-assigned ids** for types
/// outside the group.
///
/// `outside` maps a type index to whatever identity the caller is keying by. Two callers use this with
/// different notions of identity and both need the same normalisation:
///
/// * [`canonicalize`] passes the module-local canonical ids, giving keys comparable **within** a module.
/// * [`crate::interp::Store`]'s type registry passes the **store-wide** ids of the module's earlier
///   groups, giving keys comparable **across** modules — which is the whole point of the registry, and
///   is why this is parameterised rather than reading `canon` directly.
pub(crate) fn rec_group_key_with(
    comp: &[CompType],
    supers: &[Option<u32>],
    finals: &[bool],
    outside: &[u32],
    start: u32,
    len: u32,
) -> Vec<u8> {
    rec_group_key(comp, supers, finals, outside, start, len)
}

/// The structural key of one rec group: every member in order, with type references normalised.
fn rec_group_key(
    comp: &[CompType],
    supers: &[Option<u32>],
    finals: &[bool],
    canon: &[u32],
    start: u32,
    len: u32,
) -> Vec<u8> {
    let mut k = Vec::new();
    for i in 0..len {
        let t = (start + i) as usize;
        // Finality and the declared supertype are part of a type's identity, not decoration: two
        // otherwise identical types differing in either are different types.
        k.push(u8::from(finals.get(t).copied().unwrap_or(true)));
        match supers.get(t).copied().flatten() {
            Some(s) => {
                k.push(1);
                push_type_ref(&mut k, canon, start, len, s);
            }
            None => k.push(0),
        }
        match comp.get(t) {
            Some(CompType::Func(ft)) => {
                k.push(0x60);
                push_u32(&mut k, ft.params.len() as u32);
                for &v in &ft.params {
                    push_val_type(&mut k, canon, start, len, v);
                }
                push_u32(&mut k, ft.results.len() as u32);
                for &v in &ft.results {
                    push_val_type(&mut k, canon, start, len, v);
                }
            }
            Some(CompType::Struct(fs)) => {
                k.push(0x5f);
                push_u32(&mut k, fs.len() as u32);
                for f in fs {
                    push_field_type(&mut k, canon, start, len, f);
                }
            }
            Some(CompType::Array(f)) => {
                k.push(0x5e);
                push_field_type(&mut k, canon, start, len, f);
            }
            // Cannot happen for a decoded module; keyed distinctly rather than assumed away.
            None => k.push(0xff),
        }
    }
    k
}

fn push_u32(k: &mut Vec<u8>, v: u32) {
    k.extend_from_slice(&v.to_le_bytes());
}

fn push_field_type(k: &mut Vec<u8>, canon: &[u32], start: u32, len: u32, f: &FieldType) {
    k.push(u8::from(f.mutable));
    match f.storage {
        StorageType::I8 => k.push(0x78),
        StorageType::I16 => k.push(0x77),
        StorageType::Val(v) => push_val_type(k, canon, start, len, v),
    }
}

/// A value type, with a **concrete** reference reduced to (nullability, normalised target) so the
/// module-local type index it packs cannot leak into the key.
fn push_val_type(k: &mut Vec<u8>, canon: &[u32], start: u32, len: u32, v: ValType) {
    if v.is_concrete() {
        k.push(0xc0 | u8::from(v.is_non_null_ref()));
        push_type_ref(k, canon, start, len, v.concrete_index());
    } else {
        k.push(0x00);
        push_u32(k, v.bits());
    }
}

fn push_type_ref(k: &mut Vec<u8>, canon: &[u32], start: u32, len: u32, t: u32) {
    if t >= start && t < start + len {
        // Inside this group: its POSITION, so the group's shape does not depend on where the group
        // sits in the index space. This is what makes two identical rec groups compare equal — and
        // what keeps a group that refers to its OWN member distinct from one referring outward.
        k.push(0x01);
        push_u32(k, t - start);
    } else if let Some(&c) = canon.get(t as usize) {
        k.push(0x02);
        push_u32(k, c);
    } else {
        // A reference forward, out of the group — invalid, and not canonicalisable. Keyed by its raw
        // index so two different bad modules are not accidentally equated, and so this stays total:
        // the validator reports the bad index; the decoder does not panic on it.
        k.push(0x03);
        push_u32(k, t);
    }
}

/// Decode one sub type: an optional `0x50`/`0x4f` wrapper carrying a supertype list (GC
/// MVP: at most one), then a composite type.
fn decode_sub_type(
    kinds: &[CompKind],
    r: &mut Reader,
    comp: &mut Vec<CompType>,
    supers: &mut Vec<Option<u32>>,
    finals: &mut Vec<bool>,
) -> DecodeResult<()> {
    let mut super_idx: Option<u32> = None;
    let tag = r.peek_byte()?;
    // Only `0x50` (`sub`) declares a type OPEN for extension. `0x4f` is `sub final`, and a bare
    // composite type is shorthand for `sub final ϵ` — so **final is the default**, and the two
    // wrapper bytes are not interchangeable. Decoding both as "has a supertype list" and dropping
    // the distinction is what let a module declare a final type as its supertype.
    let is_final = tag != 0x50;
    if tag == 0x50 || tag == 0x4f {
        r.read_byte()?;
        let ns = r.read_var_u32()?;
        if ns > 1 {
            return Err(DecodeError::BadType); // MVP allows at most one supertype
        }
        if ns == 1 {
            let s = r.read_var_u32()?;
            // A supertype must be a PRIOR type (lower index than this one, whose index is
            // `comp.len()` — not yet appended), so the chain strictly decreases and
            // `is_subtype`'s walk can't loop.
            if s as usize >= comp.len() {
                return Err(DecodeError::BadType);
            }
            super_idx = Some(s);
        }
    }
    comp.push(decode_comp_type(kinds, r)?);
    supers.push(super_idx);
    finals.push(is_final);
    Ok(())
}

/// Decode a composite type: `0x60` func / `0x5f` struct / `0x5e` array.
fn decode_comp_type(kinds: &[CompKind], r: &mut Reader) -> DecodeResult<CompType> {
    match r.read_byte()? {
        0x60 => {
            let params = read_val_types(r, kinds)?;
            let results = read_val_types(r, kinds)?;
            Ok(CompType::Func(FuncType { params, results }))
        }
        0x5f => {
            let n = r.read_vec_len()?;
            let mut fs = Vec::with_capacity(n as usize);
            for _ in 0..n {
                fs.push(read_field_type(r, kinds)?);
            }
            Ok(CompType::Struct(fs))
        }
        0x5e => Ok(CompType::Array(read_field_type(r, kinds)?)),
        _ => Err(DecodeError::BadType),
    }
}

// --- Type-kind pre-scan (pass A): record each type's kind without resolving inner
// reference types (whose family may forward-reference a later type). Mirrors pass B.

fn prescan_type_kinds(r: &mut Reader) -> DecodeResult<Vec<CompKind>> {
    let mut kinds: Vec<CompKind> = Vec::new();
    let mut nrec = r.read_var_u32()?;
    while nrec > 0 {
        nrec -= 1;
        if r.peek_byte()? == 0x4e {
            r.read_byte()?;
            let mut k = r.read_var_u32()?;
            while k > 0 {
                k -= 1;
                scan_sub_type(r, &mut kinds)?;
            }
        } else {
            scan_sub_type(r, &mut kinds)?;
        }
    }
    Ok(kinds)
}

fn scan_sub_type(r: &mut Reader, kinds: &mut Vec<CompKind>) -> DecodeResult<()> {
    let tag = r.peek_byte()?;
    if tag == 0x50 || tag == 0x4f {
        r.read_byte()?;
        let mut ns = r.read_var_u32()?;
        while ns > 0 {
            ns -= 1;
            r.read_var_u32()?; // supertype indices
        }
    }
    match r.read_byte()? {
        0x60 => {
            skip_val_type_vec(r)?;
            skip_val_type_vec(r)?;
            kinds.push(CompKind::Func);
        }
        0x5f => {
            let mut n = r.read_var_u32()?;
            while n > 0 {
                n -= 1;
                skip_field_type(r)?;
            }
            kinds.push(CompKind::Struct);
        }
        0x5e => {
            skip_field_type(r)?;
            kinds.push(CompKind::Array);
        }
        _ => return Err(DecodeError::BadType),
    }
    Ok(())
}

fn skip_val_type_vec(r: &mut Reader) -> DecodeResult<()> {
    let mut n = r.read_var_u32()?;
    while n > 0 {
        n -= 1;
        skip_val_type(r)?;
    }
    Ok(())
}

/// Advance past one value type without resolving it (bytes only).
fn skip_val_type(r: &mut Reader) -> DecodeResult<()> {
    let b = r.read_byte()?;
    if b == 0x63 || b == 0x64 {
        r.read_var_s33()?; // (ref null? ht): + heaptype s33
    }
    Ok(())
}

fn skip_field_type(r: &mut Reader) -> DecodeResult<()> {
    let b = r.peek_byte()?;
    if b == 0x77 || b == 0x78 {
        r.read_byte()?; // packed i16 / i8
    } else {
        skip_val_type(r)?;
    }
    r.read_byte()?; // mutability
    Ok(())
}

fn func_type_at(d: &Decoder, type_index: u32) -> DecodeResult<FuncType> {
    match d
        .comp_types
        .get(type_index as usize)
        .ok_or(DecodeError::IndexOutOfRange)?
    {
        CompType::Func(f) => Ok(f.clone()),
        _ => Err(DecodeError::BadType),
    }
}

fn decode_import_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<Vec<Import>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let module = read_name(r)?;
        let name = read_name(r)?;
        let kind = ExternKind::from_u8(r.read_byte()?).ok_or(DecodeError::UnknownExternKind)?;
        let mut func_type_index = None;
        let ty = match kind {
            ExternKind::Func => {
                let ti = r.read_var_u32()?;
                let ft = func_type_at(d, ti)?;
                d.func_space.push(ft.clone());
                // Kept alongside the resolved signature: identity questions need the index, not
                // the structure. See `Import::func_type_index`.
                func_type_index = Some(ti);
                Extern::Func(ft)
            }
            ExternKind::Table => {
                // An imported table never carries an initializer — the exporter owns the
                // contents — so this is always the plain form.
                let tt = read_table_type(r, &d.type_kinds)?;
                d.table_space.push(tt.clone());
                Extern::Table(tt)
            }
            ExternKind::Memory => {
                let mt = MemoryType {
                    limits: read_limits(r)?,
                };
                d.mem_space.push(mt);
                Extern::Memory(mt)
            }
            ExternKind::Global => {
                let gt = read_global_type(r, &d.type_kinds)?;
                d.global_space.push(gt);
                Extern::Global(gt)
            }
            ExternKind::Tag => {
                // Tag import: an attribute byte (0 = exception) + a type index.
                if r.read_byte()? != 0x00 {
                    return Err(DecodeError::MalformedFlag);
                }
                let ti = r.read_var_u32()?;
                let ft = func_type_at(d, ti)?;
                d.tag_space.push(ft.clone());
                // ⚠️ Keep the INDEX as well as the structure, for the same reason a function import
                // does: §4.5.9 matches a tag by its defined TYPE, and two tags can both be `(func)`
                // and still be different types — rec-group membership is part of identity and only
                // the index carries it. `tag.wast`'s link-time typing section is exactly that case.
                func_type_index = Some(ti);
                Extern::Tag(ft)
            }
        };
        list.push(Import {
            module,
            name,
            ty,
            func_type_index,
        });
    }
    Ok(list)
}

fn decode_function_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<Vec<u32>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let ti = r.read_var_u32()?;
        let ft = func_type_at(d, ti)?;
        d.func_space.push(ft);
        list.push(ti);
    }
    Ok(list)
}

/// Tag section (§5.5.14, EH): each tag is an attribute byte (0x00 = exception) + a type
/// index. Returns the type indices.
fn decode_tag_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<Vec<u32>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let attr = r.read_byte()?;
        if attr != 0x00 {
            return Err(DecodeError::MalformedFlag);
        }
        let ti = r.read_var_u32()?;
        let ft = func_type_at(d, ti)?;
        d.tag_space.push(ft);
        list.push(ti);
    }
    Ok(list)
}

fn decode_table_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<()> {
    let mut count = r.read_var_u32()?;
    while count > 0 {
        count -= 1;
        // §5.5.6 with function-references, two forms:
        //   tabletype                       — entries start null
        //   0x40 0x00 tabletype expr        — entries start as `expr`
        // `0x40` cannot begin a valtype, so peeking one byte disambiguates. Reading the
        // second form is what stops a table declared with an initializer from silently
        // instantiating full of nulls.
        let mut tt = if r.peek_byte()? == 0x40 {
            r.read_byte()?; // 0x40
            if r.read_byte()? != 0x00 {
                return Err(DecodeError::MalformedFlag);
            }
            let mut tt = read_table_type(r, &d.type_kinds)?;
            tt.init = Some(read_const_expr_bytes(r)?);
            tt
        } else {
            read_table_type(r, &d.type_kinds)?
        };
        // A non-nullable element type is uninhabited without an initializer, so the plain
        // form cannot express it. Rejected here rather than producing a table of nulls
        // typed as non-null.
        if tt.init.is_none() && tt.element.is_non_null_ref() {
            return Err(DecodeError::MalformedFlag);
        }
        tt.limits.shared = false;
        d.table_space.push(tt);
    }
    Ok(())
}

fn decode_memory_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<()> {
    let mut count = r.read_var_u32()?;
    while count > 0 {
        count -= 1;
        let mt = MemoryType {
            limits: read_limits(r)?,
        };
        d.mem_space.push(mt);
    }
    Ok(())
}

fn decode_global_section(d: &mut Decoder, r: &mut Reader) -> DecodeResult<()> {
    let mut count = r.read_var_u32()?;
    while count > 0 {
        count -= 1;
        let gt = read_global_type(r, &d.type_kinds)?;
        d.global_space.push(gt);
        let init = read_const_expr_bytes(r)?;
        d.global_init_space.push(init);
    }
    Ok(())
}

fn decode_export_section(d: &Decoder, r: &mut Reader) -> DecodeResult<Vec<Export>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_name(r)?;
        let kind = ExternKind::from_u8(r.read_byte()?).ok_or(DecodeError::UnknownExternKind)?;
        let index = r.read_var_u32()?;
        let ty = match kind {
            ExternKind::Func => Extern::Func(space_at(&d.func_space, index)?),
            ExternKind::Table => Extern::Table(space_at(&d.table_space, index)?),
            ExternKind::Memory => Extern::Memory(space_at(&d.mem_space, index)?),
            ExternKind::Global => Extern::Global(space_at(&d.global_space, index)?),
            ExternKind::Tag => Extern::Tag(space_at(&d.tag_space, index)?),
        };
        list.push(Export { name, index, ty });
    }
    Ok(list)
}

fn space_at<T: Clone>(space: &[T], index: u32) -> DecodeResult<T> {
    space
        .get(index as usize)
        .cloned()
        .ok_or(DecodeError::IndexOutOfRange)
}

fn decode_locals(r: &mut Reader, kinds: &[CompKind]) -> DecodeResult<Vec<Local>> {
    let n = r.read_vec_len()?;
    let mut locals = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let count = r.read_var_u32()?;
        let ty = read_val_type(r, kinds)?;
        locals.push(Local { count, ty });
    }
    Ok(locals)
}

/// Copy a length-prefixed byte vector into owned memory.
fn read_byte_vec(r: &mut Reader) -> DecodeResult<Vec<u8>> {
    let n = r.read_var_u32()? as usize;
    Ok(r.read_bytes(n)?.to_vec())
}

/// Capture the raw bytes of a constant expression (through its `end`).
fn read_const_expr_bytes(r: &mut Reader) -> DecodeResult<Vec<u8>> {
    let start = r.pos();
    skip_const_expr(r)?;
    Ok(r.input()[start..r.pos()].to_vec())
}

fn read_func_vec(r: &mut Reader) -> DecodeResult<Vec<u32>> {
    let n = r.read_vec_len()?;
    let mut funcs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        funcs.push(r.read_var_u32()?);
    }
    Ok(funcs)
}

/// Read a vector of element const-expressions (each terminated by `end`).
fn read_expr_vec(r: &mut Reader) -> DecodeResult<Vec<Vec<u8>>> {
    let n = r.read_vec_len()?;
    let mut exprs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        exprs.push(read_const_expr_bytes(r)?);
    }
    Ok(exprs)
}

/// Decode the element section (§5.5.12): all 8 flag variants.
fn decode_element_section(d: &Decoder, r: &mut Reader) -> DecodeResult<Vec<Element>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let flags = r.read_var_u32()?;
        let mut table_index = 0u32;
        let mut offset_expr: Vec<u8> = Vec::new();
        let mut funcs: Vec<u32> = Vec::new();
        let mut exprs: Vec<Vec<u8>> = Vec::new();
        let mut elem_type = ValType::FUNCREF;
        // bit0: 0 = active; bit1 (when bit0=1): 0 = passive, 1 = declarative.
        let mode = if flags & 0b001 == 0 {
            ElementMode::Active
        } else if flags & 0b010 == 0 {
            ElementMode::Passive
        } else {
            ElementMode::Declarative
        };
        // bit1 (of active) selects an explicit table index; bit2 selects the expr form.
        if mode == ElementMode::Active && (flags & 0b010) != 0 {
            table_index = r.read_var_u32()?;
        }
        if mode == ElementMode::Active {
            offset_expr = read_const_expr_bytes(r)?;
        }
        if flags & 0b100 == 0 {
            // Func-index form. Non-flag-0 variants carry a leading elemkind byte.
            if flags != 0 {
                r.read_byte()?; // elemkind (0x00 = funcref)
            }
            funcs = read_func_vec(r)?;
            // ⚠️ §5.5.12 with function-references: the funcidx **shorthand** forms have type
            // `(ref func)` — NON-NULL — because every element is `(ref.func y)`, which can never
            // be null. The default above is the nullable `funcref`, and that difference is not
            // cosmetic: it decides whether such a segment may initialise a table declared
            // `(ref func)`. It could not, so `(table 1 (ref func) …) (elem (i32.const 0) func 0)`
            // — a **valid** module the spec ships five times in `elem.wast` — was refused as a
            // TypeMismatch. *A nullable default is the safe-looking choice and the wrong one:
            // here it under-approximates the type and rejects valid input.*
            elem_type = ValType::FUNCREF_NN;
        } else {
            // Const-expr form. Non-flag-4 variants carry a leading reftype byte.
            if flags != 4 {
                elem_type = read_val_type(r, &d.type_kinds)?;
                // ⚠️ §5.5.12 spells this field **reftype**, not valtype — so `\7f` (i32) here is
                // MALFORMED, and the suite says so ("malformed reference type"). Reading it as a
                // valtype accepted the module and left the complaint to the validator, which
                // reported a `TypeMismatch` at a completely different stage. **Rejection STAGE is
                // part of correctness** (§3.6): `assert_malformed` is not satisfied by an
                // `assert_invalid`-shaped answer, and a caller told "invalid" reasonably concludes
                // the bytes were well-formed.
                if !elem_type.is_ref() {
                    return Err(DecodeError::BadValType);
                }
            }
            exprs = read_expr_vec(r)?;
        }
        list.push(Element {
            mode,
            table_index,
            offset_expr,
            funcs,
            exprs,
            elem_type,
        });
    }
    Ok(list)
}

fn decode_data_section(r: &mut Reader) -> DecodeResult<Vec<DataSegment>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let seg = match r.read_var_u32()? {
            // segment flags (§5.5.14)
            0 => {
                let offset_expr = read_const_expr_bytes(r)?;
                let bytes = read_byte_vec(r)?;
                DataSegment { active: true, mem_index: 0, offset_expr, bytes }
            }
            1 => DataSegment {
                active: false,
                mem_index: 0,
                offset_expr: Vec::new(),
                bytes: read_byte_vec(r)?,
            },
            2 => {
                let mem_index = r.read_var_u32()?;
                let offset_expr = read_const_expr_bytes(r)?;
                let bytes = read_byte_vec(r)?;
                DataSegment { active: true, mem_index, offset_expr, bytes }
            }
            _ => return Err(DecodeError::UnsupportedOpcode),
        };
        list.push(seg);
    }
    Ok(list)
}

/// `payload_base` is the code section payload's absolute offset in the module, so each
/// body can record where its bytes live in the original binary.
/// Every constant expression a module carries, as raw byte slices: global initializers, table
/// initializers (function-references), element-segment offsets and element expressions, and
/// data-segment offsets.
///
/// One list so the decode-time encoding check cannot miss a kind — the alternative, five separate
/// loops at the call site, is exactly the shape that grows a sixth const-expr field and forgets it.
fn module_const_exprs<'a>(
    d: &'a Decoder,
    elements: &'a [Element],
    data: &'a [DataSegment],
) -> impl Iterator<Item = &'a [u8]> {
    // Keyed on the segment MODE, not on whether the byte string is empty: a *passive* segment has
    // no offset expression at all, while an *active* one whose expression is empty is genuinely
    // malformed. Filtering by `is_empty()` would conflate the two and silently excuse the second.
    d.global_init_space
        .iter()
        .map(Vec::as_slice)
        .chain(d.table_space.iter().filter_map(|t| t.init.as_deref()))
        .chain(elements.iter().flat_map(|e| {
            (e.mode == ElementMode::Active)
                .then_some(e.offset_expr.as_slice())
                .into_iter()
                .chain(e.exprs.iter().map(Vec::as_slice))
        }))
        .chain(
            data.iter()
                .filter(|s| s.active)
                .map(|s| s.offset_expr.as_slice()),
        )
}

/// The spec ceiling on a function's total locals: the count must be representable, i.e. at most
/// 2^32−1 (§5.4.5 / `binary.wast` "too many locals"). Deliberately **not** the validator's
/// `MAX_LOCALS` resource cap, which is far smaller: the two say different things — this one is
/// "these bytes cannot mean anything", that one is "we decline to allocate that much".
const MAX_DECLARED_LOCALS: u64 = u32::MAX as u64;

fn decode_code_section(d: &Decoder, r: &mut Reader, payload_base: usize) -> DecodeResult<Vec<Code>> {
    let count = r.read_vec_len()?;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        // Each entry is a byte-counted (locals ++ body) blob; decode within it so a
        // malformed local vector can't run past the entry.
        let entry_len = r.read_var_u32()? as usize;
        let entry_start = r.pos();
        let entry = r.read_bytes(entry_len)?;
        let mut er = Reader::new(entry);
        let locals = decode_locals(&mut er, &d.type_kinds)?;
        // Too many declared locals is a *malformed* encoding, not a typing fact (the count cannot
        // be represented), so it belongs here. Summed as `u64` before anything is allocated, so a
        // run of `0xFFFFFFFF` costs a comparison rather than an allocation.
        let declared: u64 = locals.iter().map(|l| u64::from(l.count)).sum();
        if declared > MAX_DECLARED_LOCALS {
            return Err(DecodeError::TooManyLocals);
        }
        // Decode the body HERE, borrowing the entry rather than copying it out first. A malformed
        // instruction stream is a decode error by the spec, and deferring it meant `decode`
        // accepted modules the validator then rejected — the wrong stage, and measurably so
        // (`binary-leb128.wast` reported 15 such assertions). Doing it once also replaces two later
        // decodes of the same bytes (the validator's, and each instantiation's).
        let ir = crate::opcode::decode_body(&entry[er.pos()..])?;
        // The body starts `er.pos()` into the entry, which began at `entry_start`.
        // Saturate rather than truncate (only used to label a trap backtrace).
        let body_offset = u32::try_from(payload_base + entry_start + er.pos()).unwrap_or(u32::MAX);
        list.push(Code {
            locals,
            ir,
            body_offset,
        });
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Prepend the wasm magic + version to a section byte sequence.
    fn m(rest: &[u8]) -> Vec<u8> {
        let mut v = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        v.extend_from_slice(rest);
        v
    }

    /// §5.5.12 spells an element segment's explicit type field **reftype**, not valtype — so a
    /// numeric type byte there is MALFORMED, and must be refused at DECODE.
    ///
    /// ⚠️ It was read as a valtype, so the module decoded and the complaint fell to the
    /// validator as a `TypeMismatch` — a different stage, and one that tells a caller the bytes
    /// were well-formed when they were not. **Rejection stage is part of correctness.**
    #[test]
    fn an_element_segments_type_field_must_be_a_reference_type() {
        // Element section, flags 5 (passive, const-expr form with an explicit type), then the
        // type byte, then one `ref.func 0` expression.
        let elem = |ty: u8| m(&[0x09, 0x07, 0x01, 0x05, ty, 0x01, 0xd2, 0x00, 0x0b]);
        // `0x7f` is i32 — not a reference type.
        assert_eq!(decode(&elem(0x7f)), Err(crate::types::DecodeError::BadValType));
        // `0x70` is funcref, and must still decode — so this did not just start refusing the field.
        assert!(decode(&elem(0x70)).is_ok());
    }

    // --- §5.5.2 section structure: order, uniqueness, declared size (2026-08-08) ---

    /// A repeated section used to **silently replace** the first occurrence, because each arm
    /// assigns (`functions = decode_function_section(…)`). That is the silent-wrong-output class:
    /// the module that ran was not the module on disk.
    #[test]
    fn rejects_a_repeated_section() {
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type
            0x03, 0x02, 0x01, 0x00, // function: 1 func
            0x03, 0x02, 0x01, 0x00, // function AGAIN
            0x0a, 0x07, 0x02, 0x02, 0x00, 0x0b, 0x02, 0x00, 0x0b, // code: 2 bodies
        ]);
        assert_eq!(decode(&bytes), Err(DecodeError::SectionOrder));
        // Mutation guard: without the repeat it decodes — but as ONE function against TWO bodies,
        // so it is then caught by the func/code count check rather than accepted.
        let one = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x07, 0x02, 0x02, 0x00, 0x0b, 0x02, 0x00, 0x0b,
        ]);
        assert_eq!(decode(&one), Err(DecodeError::FuncCodeCountMismatch));
    }

    #[test]
    fn rejects_sections_out_of_order() {
        // Export (7) after code (10). Ordinary producers never emit this; a hostile one might.
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
            0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00,
        ]);
        assert_eq!(decode(&bytes), Err(DecodeError::SectionOrder));
    }

    /// The order is **not** the id order, so this is the case a `>` on raw ids gets backwards:
    /// `DataCount` is id 12 yet must precede `Code` (id 10), and `Tag` is id 13 yet belongs
    /// between `Memory` and `Global`. Both are accepted here in their correct positions.
    #[test]
    fn accepts_data_count_before_code_and_tag_before_global() {
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type (1)
            0x03, 0x02, 0x01, 0x00, // function (3)
            0x05, 0x03, 0x01, 0x00, 0x01, // memory (5)
            0x0d, 0x03, 0x01, 0x00, 0x00, // tag (13) — before global
            0x06, 0x06, 0x01, 0x7f, 0x00, 0x41, 0x00, 0x0b, // global (6)
            0x0c, 0x01, 0x01, // data count (12) — before code
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // code (10)
            0x0b, 0x07, 0x01, 0x00, 0x41, 0x00, 0x0b, 0x01, 0x2a, // data (11)
        ]);
        let md = decode(&bytes).expect("the canonical order must decode");
        assert_eq!(md.tags.len(), 1);
        assert_eq!(md.data.len(), 1);
        // And the same two, moved to where their raw ids would put them, are refused.
        let tag_last = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x06, 0x06, 0x01, 0x7f, 0x00, 0x41, 0x00, 0x0b, // global (6)
            0x0d, 0x03, 0x01, 0x00, 0x00, // tag (13) AFTER global — id order, wrong order
        ]);
        assert_eq!(decode(&tag_last), Err(DecodeError::SectionOrder));
    }

    /// Custom sections are exempt: any number, anywhere.
    #[test]
    fn custom_sections_may_repeat_and_appear_anywhere() {
        let bytes = m(&[
            0x00, 0x03, 0x02, b'h', b'i', // custom
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type
            0x00, 0x03, 0x02, b'h', b'i', // custom again, mid-module
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
            0x00, 0x03, 0x02, b'h', b'i', // and after the last real section
        ]);
        assert!(decode(&bytes).is_ok());
    }

    /// A section that under-reads its declared size left the leftover bytes simply *absent* from
    /// the decoded module — the outer reader had already skipped them.
    #[test]
    fn rejects_a_section_whose_contents_are_shorter_than_its_declared_size() {
        // Type section declares size 5 but its contents occupy 4.
        let bytes = m(&[0x01, 0x05, 0x01, 0x60, 0x00, 0x00, 0x00]);
        assert_eq!(decode(&bytes), Err(DecodeError::SectionSizeMismatch));
        // The honest size decodes.
        assert!(decode(&m(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00])).is_ok());
    }

    #[test]
    fn rejects_function_code_count_mismatch_at_decode() {
        // A function section with no code section at all — malformed (§5.5.13), and previously
        // only caught one stage later by the validator.
        let bytes = m(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00]);
        assert_eq!(decode(&bytes), Err(DecodeError::FuncCodeCountMismatch));
        // Neither section is fine: not every module has functions.
        assert!(decode(&m(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00])).is_ok());
    }

    /// A malformed *instruction encoding* is a decode error, not a validation one. Bodies used to
    /// be decoded lazily, so `decode` accepted these and the validator reported them — which is
    /// what `binary-leb128.wast` was measuring when it showed 15 wrong-stage rejections.
    #[test]
    fn rejects_a_malformed_body_encoding_at_decode() {
        // `i32.const` with an over-long LEB (unused bits set): malformed, not ill-typed.
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x0a, 0x0b, 0x01, 0x09, 0x00, 0x41, 0x80, 0x80, 0x80, 0x80, 0x70, 0x1a, 0x0b,
        ]);
        assert_eq!(decode(&bytes), Err(DecodeError::LebOverflow));
    }

    /// The same rule for constant expressions, which are stored as raw bytes and were read by the
    /// validator and the interpreter with their own little readers.
    #[test]
    fn rejects_a_malformed_const_expr_encoding_at_decode() {
        // Global section: i32 immutable, `i32.const` with unused bits set.
        let bytes = m(&[0x06, 0x0a, 0x01, 0x7f, 0x00, 0x41, 0x80, 0x80, 0x80, 0x80, 0x70, 0x0b]);
        assert_eq!(decode(&bytes), Err(DecodeError::LebOverflow));
        // A well-formed one decodes.
        assert!(decode(&m(&[0x06, 0x06, 0x01, 0x7f, 0x00, 0x41, 0x00, 0x0b])).is_ok());
    }

    /// A **passive** data segment has no offset expression at all, so the const-expr sweep must key
    /// on the segment's mode. Keying on "is the byte string empty" instead would also excuse an
    /// *active* segment whose offset is genuinely missing. This test is why that distinction is in
    /// the code: written the sloppy way, it fails.
    #[test]
    fn a_passive_segment_has_no_offset_expression_to_check() {
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type
            0x03, 0x02, 0x01, 0x00, // function
            0x05, 0x03, 0x01, 0x00, 0x01, // memory
            0x0c, 0x01, 0x01, // data count 1 — required by memory.init below
            // code: memory.init 0 0 with three zero operands
            0x0a, 0x0e, 0x01, 0x0c, 0x00, 0x41, 0x00, 0x41, 0x00, 0x41, 0x00, 0xfc, 0x08, 0x00,
            0x00, 0x0b,
            0x0b, 0x05, 0x01, 0x01, 0x02, b'h', b'i', // data: PASSIVE (flag 1), no offset expr
        ]);
        let md = decode(&bytes).expect("a passive segment must decode");
        assert!(!md.data[0].active);
        assert!(md.data[0].offset_expr.is_empty());
    }

    /// `memory.init` / `data.drop` need the data-count section: it is what lets their segment index
    /// be checked without having read the data section, so its absence is malformed (bulk-memory).
    #[test]
    fn rejects_a_bulk_data_op_with_no_data_count_section() {
        let body = [
            0x0a, 0x0e, 0x01, 0x0c, 0x00, 0x41, 0x00, 0x41, 0x00, 0x41, 0x00, 0xfc, 0x08, 0x00,
            0x00, 0x0b,
        ];
        let mut bytes = vec![
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x05, 0x03, 0x01, 0x00, 0x01,
        ];
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&[0x0b, 0x05, 0x01, 0x01, 0x02, b'h', b'i']);
        assert_eq!(decode(&m(&bytes)), Err(DecodeError::DataCountRequired));
        // With the section present (id 12, before code) it decodes — see the test above.
    }

    #[test]
    fn decodes_empty_module() {
        let md = decode(&m(&[])).unwrap();
        assert_eq!(md.version, 1);
        assert_eq!(md.sections.len(), 0);
        assert_eq!(md.comp_types.len(), 0);
    }

    #[test]
    fn indexes_a_custom_section() {
        // custom: id 0, size 1, payload = name-length 0.
        let md = decode(&m(&[0x00, 0x01, 0x00])).unwrap();
        assert_eq!(md.sections.len(), 1);
        let s = md.section(SectionId::Custom).unwrap();
        assert_eq!(s.size, 1);
        assert_eq!(s.offset, 10);
    }

    #[test]
    fn names_must_be_valid_utf8() {
        // custom-section id = a lone continuation byte (0x80) — never valid UTF-8.
        assert_eq!(
            decode(&m(&[0x00, 0x02, 0x01, 0x80])),
            Err(DecodeError::InvalidUtf8)
        );
        // The same framing with a valid 1-byte name decodes.
        assert!(decode(&m(&[0x00, 0x02, 0x01, b'x'])).is_ok());
        // An EXPORT name that is a truncated 2-byte UTF-8 sequence is rejected.
        let bytes = m(&[
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type [] -> []
            0x03, 0x02, 0x01, 0x00, // function: one func of type 0
            0x07, 0x05, 0x01, 0x01, 0xC3, 0x00, 0x00, // export "\xC3" func 0
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // code: one empty body
        ]);
        assert_eq!(decode(&bytes), Err(DecodeError::InvalidUtf8));
    }

    #[test]
    fn rejects_bad_magic() {
        assert_eq!(
            decode(&[b'n', b'o', b'p', b'e', 0x01, 0x00, 0x00, 0x00]),
            Err(DecodeError::BadMagic)
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        assert_eq!(
            decode(&[0x00, 0x61, 0x73, 0x6d, 0x02, 0x00, 0x00, 0x00]),
            Err(DecodeError::UnsupportedVersion)
        );
    }

    #[test]
    fn rejects_undefined_valtype() {
        // type section: one func type with a param byte 0x50 (not a valtype).
        let bytes = m(&[0x01, 0x05, 0x01, 0x60, 0x01, 0x50, 0x00]);
        assert_eq!(decode(&bytes), Err(DecodeError::BadValType));
    }

    #[test]
    fn rejects_reserved_global_mutability() {
        // global i32 with mutability byte 0x02 (only 0/1 valid).
        let bytes = m(&[0x06, 0x04, 0x01, 0x7f, 0x02, 0x0b]);
        assert_eq!(decode(&bytes), Err(DecodeError::MalformedFlag));
    }

    #[test]
    fn rejects_self_referential_supertype() {
        // one type = sub (0x50) with supertype [0] (itself), func [] -> [].
        let bytes = m(&[0x01, 0x07, 0x01, 0x50, 0x01, 0x00, 0x60, 0x00, 0x00]);
        assert_eq!(decode(&bytes), Err(DecodeError::BadType));
    }

    #[test]
    fn decodes_type_import_function_export() {
        // (i32,i32)->i32 ; import env.add ; one defined func of type 0 ; export "run" func 1.
        let bytes = m(&[
            0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
            0x02, 0x0b, 0x01, 0x03, b'e', b'n', b'v', 0x03, b'a', b'd', b'd', 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
            // A code section is now REQUIRED alongside the function section (§5.5.13) — this
            // fixture omitted it, which the func/code count check correctly calls malformed.
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(md.sections.len(), 5);
        assert_eq!(md.comp_types.len(), 1);
        assert_eq!(md.func_sig(0).unwrap().params, vec![ValType::I32, ValType::I32]);
        assert_eq!(md.func_sig(0).unwrap().results, vec![ValType::I32]);

        assert_eq!(md.imports.len(), 1);
        assert_eq!(md.imports[0].module, "env");
        assert_eq!(md.imports[0].name, "add");
        assert_eq!(md.imports[0].ty.kind(), ExternKind::Func);

        assert_eq!(md.functions, vec![0]);

        assert_eq!(md.exports.len(), 1);
        assert_eq!(md.exports[0].name, "run");
        assert_eq!(md.exports[0].index, 1);
        // export "run" resolves to the (i32,i32)->i32 signature of defined func 1.
        let Extern::Func(ft) = &md.exports[0].ty else {
            panic!("expected a func export");
        };
        assert_eq!(ft.results, vec![ValType::I32]);
        // and func_type resolves the same across the imports-then-defined index space.
        assert_eq!(md.func_type(1).unwrap().results, vec![ValType::I32]);
    }

    #[test]
    fn decodes_gc_struct_and_array() {
        // type section, 2 types: struct{(mut i32), i64}; array{(mut i8)}.
        let payload = [0x02, 0x5f, 0x02, 0x7f, 0x01, 0x7e, 0x00, 0x5e, 0x78, 0x01];
        let mut section = vec![0x01, payload.len() as u8];
        section.extend_from_slice(&payload);
        let md = decode(&m(&section)).unwrap();
        assert_eq!(md.comp_types.len(), 2);
        let st = md.struct_fields(0).unwrap();
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].storage, StorageType::Val(ValType::I32));
        assert!(st[0].mutable);
        assert_eq!(st[1].storage, StorageType::Val(ValType::I64));
        assert!(!st[1].mutable);
        let arr = md.array_field(1).unwrap();
        assert_eq!(arr.storage, StorageType::I8);
        assert!(arr.mutable && arr.storage.is_packed());
        assert_eq!(arr.storage.unpacked(), ValType::I32);
    }

    #[test]
    fn gc_rec_group_forward_reference() {
        // (rec (struct (field (ref 1))) (struct (field i32)))
        let payload = [
            0x01, 0x4e, 0x02, 0x5f, 0x01, 0x64, 0x01, 0x00, 0x5f, 0x01, 0x7f, 0x00,
        ];
        let mut section = vec![0x01, payload.len() as u8];
        section.extend_from_slice(&payload);
        let md = decode(&m(&section)).unwrap();
        assert_eq!(md.comp_types.len(), 2);
        let StorageType::Val(f0) = md.struct_fields(0).unwrap()[0].storage else {
            panic!("expected a value-typed field");
        };
        assert!(f0.is_concrete() && f0.is_non_null_ref());
        assert_eq!(f0.concrete_index(), 1);
        assert_eq!(f0.ref_heap(), RefHeap::Struct);
        assert_eq!(
            md.struct_fields(1).unwrap()[0].storage,
            StorageType::Val(ValType::I32)
        );
    }

    #[test]
    fn decodes_code_section() {
        // (func (param i32 i32) (result i32) (local i32) local.get 0 local.get 1 i32.add)
        // Sections in the order §5.5.2 fixes — export (7) BEFORE code (10). This fixture had them
        // reversed until the section-order check was added and refused it.
        let bytes = m(&[
            0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // type
            0x03, 0x02, 0x01, 0x00, // function: 1 func of type 0
            0x07, 0x07, 0x01, 0x03, b'a', b'd', b'd', 0x00, 0x00, // export
            0x0a, 0x0b, 0x01, 0x09, 0x01, 0x01, 0x7f, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b, // code
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(md.code.len(), 1);
        assert_eq!(md.code[0].locals.len(), 1);
        assert_eq!(md.code[0].locals[0].count, 1);
        assert_eq!(md.code[0].locals[0].ty, ValType::I32);
        assert_eq!(md.code[0].local_count(), 1);
        // The body is now stored decoded, so assert on the instructions rather than the bytes —
        // which is the stronger assertion anyway: it checks the decode, not just the slicing.
        assert_eq!(
            md.code[0].ir.iter().map(|i| i.op).collect::<Vec<_>>(),
            vec![
                crate::opcode::Op::LocalGet,
                crate::opcode::Op::LocalGet,
                crate::opcode::Op::I32Add,
                crate::opcode::Op::End,
            ]
        );
    }

    #[test]
    fn resolves_memory_export_with_limits() {
        // memory: count 1, flag 1 (has max), min 1, max 2; export "mem" memory 0.
        let bytes = m(&[
            0x05, 0x04, 0x01, 0x01, 0x01, 0x02,
            0x07, 0x07, 0x01, 0x03, b'm', b'e', b'm', 0x02, 0x00,
        ]);
        let md = decode(&bytes).unwrap();
        assert_eq!(md.exports.len(), 1);
        let Extern::Memory(mt) = &md.exports[0].ty else {
            panic!("expected a memory export");
        };
        assert_eq!(mt.limits.min, 1);
        assert_eq!(mt.limits.max, Some(2));
    }

    #[test]
    fn reads_function_names_from_name_section() {
        // custom "name": subsection 0 (module name, skipped) + subsection 1 (func names).
        let namemap = [0x02, 0x00, 0x02, b'h', b'i', 0x03, 0x03, b'b', b'y', b'e'];
        let mut payload = vec![0x00, 0x16, 0x04, b'n', b'a', b'm', b'e'];
        payload.extend_from_slice(&[0x00, 0x03, 0x02, b'm', b'd']); // subsection 0
        payload.extend_from_slice(&[0x01, 0x0a]); // subsection 1, size 10
        payload.extend_from_slice(&namemap);
        let md = decode(&m(&payload)).unwrap();
        assert_eq!(md.func_name(0), Some(&b"hi"[..]));
        assert_eq!(md.func_name(3), Some(&b"bye"[..]));
        assert_eq!(md.func_name(1), None); // gap in the vec
        assert_eq!(md.func_name(9), None); // past the end
    }

    #[test]
    fn malformed_name_section_is_not_an_error() {
        let m1 = decode(&m(&[])).unwrap();
        assert_eq!(m1.func_names, None);
        assert_eq!(m1.func_name(0), None);
        // A name section whose function subsection is truncated mid-entry must not fail.
        let truncated = m(&[
            0x00, 0x0b, 0x04, b'n', b'a', b'm', b'e', 0x01, 0x04, 0x02, 0x00, 0x05, b'h',
        ]);
        let m2 = decode(&truncated).unwrap();
        assert_eq!(m2.func_name(0), None);
    }
}
