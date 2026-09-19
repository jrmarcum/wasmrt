//! Custom annotations — what `(@custom …)`, `(@name …)` and `(@metadata.code.branch_hint …)`
//! mean, where each may stand, and what they emit.
//!
//! 🎯 **Aligned with canonical tooling, MEASURED rather than read** (wasm-tools 1.259, whose `wat`
//! crate wasmtime 48 parses text with, 2026-09-19):
//!
//! * A malformed or misplaced annotation **rejects the module** — wasm-tools raises a parse error
//!   for every `assert_malformed_custom` case in the spec suite, so a warning would diverge.
//! * `@custom` emits a custom section at its `(before|after <section>)` slot; the default is
//!   `(after last)`. Within one gap, `after` anchors precede `before` anchors, source order within
//!   each (see [`slot_before`]).
//! * `@name` overrides the `$id` in the `name` section, which wasm-tools writes for `$id`s by
//!   default — all twelve subsections, labels included.
//! * A branch hint emits `metadata.code.branch_hint` just before the code section, at the byte
//!   offset of the instruction it precedes. A hint on a NON-branch instruction is still emitted:
//!   wasm-tools accepts it, and the spec's `assert_invalid_custom` asks for a diagnostic on the
//!   *custom section*, not a rejected module — see `crate::module::branch_hint_diagnostics`.
//!
//! Everything here is keyed on source POSITION, which is why [`crate::sexpr::Annot`] records the
//! index of the item it precedes and why a `Sexpr` node's ADDRESS is the key of the per-body maps
//! (the tree is immutable while a body is encoded, so an address names one item exactly).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::{is_id, Error, Result};
use crate::sexpr::{Annot, Sexpr};

pub(super) const CUSTOM: &str = "custom";
pub(super) const NAME: &str = "name";
pub(super) const HINT: &str = "metadata.code.branch_hint";

fn err(msg: &'static str) -> Error {
    Error::Annotation(msg)
}

/// Placement anchors in **section order** (which is not id order: `tag` sits between `memory`
/// and `global`). `datacount` is deliberately absent — wasm-tools refuses it as an anchor.
const ANCHORS: [&str; 12] = [
    "type", "import", "func", "table", "memory", "tag", "global", "export", "start", "elem",
    "code", "data",
];
pub(super) const A_TYPE: usize = 0;
pub(super) const A_IMPORT: usize = 1;
pub(super) const A_FUNC: usize = 2;
pub(super) const A_TABLE: usize = 3;
pub(super) const A_MEMORY: usize = 4;
pub(super) const A_TAG: usize = 5;
pub(super) const A_GLOBAL: usize = 6;
pub(super) const A_EXPORT: usize = 7;
pub(super) const A_START: usize = 8;
pub(super) const A_ELEM: usize = 9;
pub(super) const A_CODE: usize = 10;
pub(super) const A_DATA: usize = 11;

/// `(before first)`.
pub(super) const SLOT_FIRST: u8 = 0;
/// `(after last)` — the default placement.
pub(super) const SLOT_LAST: u8 = 2 * ANCHORS.len() as u8 + 1;

/// The slot just before anchor `i`. Slots are totally ordered, so `(after func)` (slot 6) sorts
/// before `(before global)` (slot 13) with nothing in between present — which is exactly the
/// order wasm-tools emits them in.
pub(super) const fn slot_before(i: usize) -> u8 {
    (2 * i + 1) as u8
}
pub(super) const fn slot_after(i: usize) -> u8 {
    (2 * i + 2) as u8
}

/// A custom section from `@custom`, with where it goes.
pub(super) struct CustomSec {
    pub slot: u8,
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

/// Name-section subsection ids (the extended name section).
pub(super) mod sub {
    pub const MODULE: u8 = 0;
    pub const FUNC: u8 = 1;
    pub const LOCAL: u8 = 2;
    pub const LABEL: u8 = 3;
    pub const TYPE: u8 = 4;
    pub const TABLE: u8 = 5;
    pub const MEMORY: u8 = 6;
    pub const GLOBAL: u8 = 7;
    pub const ELEM: u8 = 8;
    pub const DATA: u8 = 9;
    pub const FIELD: u8 = 10;
    pub const TAG: u8 = 11;
}

/// What the module-level annotations say. Built before any field is parsed, so a malformed
/// annotation is reported ahead of anything else wrong with the module — the order the reference
/// interpreter's `assert_malformed_custom` depends on (`branch_hint.wast` pairs a duplicate hint
/// with an undefined `(type 0)`).
#[derive(Default)]
pub(super) struct Plan {
    pub customs: Vec<CustomSec>,
    pub module_name: Option<Vec<u8>>,
    /// `@name` overrides: `(subsection, index, name)`.
    pub names: Vec<(u8, u32, Vec<u8>)>,
    /// Parameter/local `@name`s: `(func index, declaration ordinal, declarations in total, name)`.
    /// The ordinal becomes a local index only once the function's signature is known — a
    /// `(type $t)` with no inline params puts the params' slots in front of every declaration.
    pub locals: Vec<(u32, u32, u32, Vec<u8>)>,
}

/// Per-kind index counters, advanced in source order. Source order IS index order: imports
/// must precede definitions (the assembler refuses otherwise), and implicit types are interned
/// after every explicit one.
#[derive(Default)]
struct Ords {
    types: u32,
    funcs: u32,
    tables: u32,
    memories: u32,
    globals: u32,
    tags: u32,
    elems: u32,
    datas: u32,
}

/// Where a list's own `@name` must stand: right after the keyword and its optional `$id`.
fn name_slot(items: &[Sexpr]) -> usize {
    1 + usize::from(items.get(1).is_some_and(is_id))
}

/// Is this the keyword of an instruction that opens a label?
fn is_block_like(s: &Sexpr) -> bool {
    matches!(s.as_atom(), Some("block" | "loop" | "if" | "try" | "try_table"))
}

/// A label `@name` at position `p` names the block-like keyword atom that precedes it — the
/// folded `(block $l? (@name …) …)` and the flat `block $l? (@name …)` are the same rule, and the
/// keyword atom is the one node both emitters can see. Returns that atom's index.
fn label_anchor(items: &[Sexpr], p: usize) -> Option<usize> {
    if p >= 1 && is_block_like(&items[p - 1]) {
        return Some(p - 1);
    }
    if p >= 2 && is_id(&items[p - 1]) && is_block_like(&items[p - 2]) {
        return Some(p - 2);
    }
    None
}

fn name_value(body: &[Sexpr]) -> Result<Vec<u8>> {
    match body {
        [Sexpr::Str(s)] if core::str::from_utf8(s).is_ok() => Ok(s.clone()),
        [Sexpr::Str(_)] => Err(err("@name annotation: malformed UTF-8 encoding")),
        _ => Err(err("@name annotation: malformed name")),
    }
}

fn hint_value(body: &[Sexpr]) -> Result<u8> {
    match body {
        [Sexpr::Str(s)] if s.len() == 1 && s[0] <= 1 => Ok(s[0]),
        _ => Err(err("@metadata.code.branch_hint annotation: invalid value")),
    }
}

/// The rules every branch hint obeys wherever it stands: a valid value, at most one per
/// position, and something after it to annotate.
fn check_hints(annots: &[Annot], len: usize) -> Result<()> {
    let mut seen: Vec<usize> = Vec::new();
    for a in annots.iter().filter(|a| a.id == HINT) {
        hint_value(&a.body)?;
        if seen.contains(&a.before) {
            return Err(err("@metadata.code.branch_hint annotation: duplicate annotation"));
        }
        seen.push(a.before);
        if a.before >= len {
            return Err(err("@metadata.code.branch_hint annotation: must precede an instruction"));
        }
    }
    Ok(())
}

/// Parse `(@custom "name" place? "data"*)`, each refusal worded as the spec suite words it.
fn parse_custom(body: &[Sexpr]) -> Result<CustomSec> {
    let Some(Sexpr::Str(name)) = body.first() else {
        return Err(err("@custom annotation: missing section name"));
    };
    if core::str::from_utf8(name).is_err() {
        return Err(err("@custom annotation: malformed UTF-8 encoding"));
    }
    let mut rest = &body[1..];
    let mut slot = SLOT_LAST;
    if let Some(Sexpr::List(place, _)) = rest.first() {
        slot = parse_place(place)?;
        rest = &rest[1..];
    }
    let mut data = Vec::new();
    for s in rest {
        match s {
            Sexpr::Str(b) => data.extend_from_slice(b),
            _ => return Err(err("@custom annotation: unexpected token")),
        }
    }
    Ok(CustomSec { slot, name: name.clone(), data })
}

fn parse_place(place: &[Sexpr]) -> Result<u8> {
    let before = match place.first().and_then(Sexpr::as_atom) {
        Some("before") => true,
        Some("after") => false,
        _ => return Err(err("@custom annotation: malformed placement")),
    };
    let [_, Sexpr::Atom(sec)] = place else {
        return Err(err("@custom annotation: malformed section kind"));
    };
    match (before, sec.as_str()) {
        (true, "first") => Ok(SLOT_FIRST),
        (false, "last") => Ok(SLOT_LAST),
        (_, s) => match ANCHORS.iter().position(|a| *a == s) {
            Some(i) if before => Ok(slot_before(i)),
            Some(i) => Ok(slot_after(i)),
            None => Err(err("@custom annotation: malformed section kind")),
        },
    }
}

/// Check every annotation in a `(module …)` form and collect what the module-level ones say.
///
/// # Errors
/// [`Error::Annotation`] for the first malformed or misplaced one.
pub(super) fn plan(items: &[Sexpr], annots: &[Annot]) -> Result<Plan> {
    let mut p = Plan::default();
    let slot = name_slot(items);
    for a in annots {
        match a.id.as_str() {
            CUSTOM => p.customs.push(parse_custom(&a.body)?),
            NAME => {
                if a.before != slot {
                    return Err(err("misplaced @name annotation"));
                }
                if p.module_name.is_some() {
                    return Err(err("@name annotation: multiple module"));
                }
                p.module_name = Some(name_value(&a.body)?);
            }
            _ => return Err(err("@metadata.code.branch_hint annotation: not in a function")),
        }
    }
    let mut ord = Ords::default();
    for f in items.get(slot..).unwrap_or(&[]) {
        field(f, &mut ord, &mut p)?;
    }
    Ok(p)
}

/// The annotations of a list that may carry its own `@name` (a definition or import
/// descriptor); hints are checked and otherwise left alone — in a constant expression
/// wasm-tools accepts and drops them, so we do too. Returns the `@name`, if any.
fn own_name(node: &Sexpr) -> Result<Option<Vec<u8>>> {
    let items = node.as_list().unwrap_or(&[]);
    let annots = node.annotations();
    check_hints(annots, items.len())?;
    let mut name = None;
    for a in annots {
        match a.id.as_str() {
            CUSTOM => return Err(err("misplaced @custom annotation")),
            NAME if a.before == name_slot(items) && name.is_none() => {
                name = Some(name_value(&a.body)?);
            }
            NAME => return Err(err("misplaced @name annotation")),
            _ => {}
        }
    }
    Ok(name)
}

/// A list's OWN annotations where none has a meaning: `@custom` and `@name` are misplaced,
/// hints are held to their general rules. Does not descend.
fn generic_own(node: &Sexpr) -> Result<()> {
    let items = node.as_list().unwrap_or(&[]);
    let annots = node.annotations();
    check_hints(annots, items.len())?;
    for a in annots {
        match a.id.as_str() {
            CUSTOM => return Err(err("misplaced @custom annotation")),
            NAME => return Err(err("misplaced @name annotation")),
            _ => {}
        }
    }
    Ok(())
}

/// [`generic_own`], applied to a whole subtree.
fn generic(node: &Sexpr) -> Result<()> {
    generic_own(node)?;
    node.as_list().unwrap_or(&[]).iter().try_for_each(generic)
}

fn next(c: &mut u32) -> u32 {
    let i = *c;
    *c += 1;
    i
}

fn named(p: &mut Plan, sub: u8, idx: u32, node: &Sexpr) -> Result<()> {
    if let Some(n) = own_name(node)? {
        p.names.push((sub, idx, n));
    }
    Ok(())
}

fn field(f: &Sexpr, ord: &mut Ords, p: &mut Plan) -> Result<()> {
    let Some(items) = f.as_list() else {
        return Ok(());
    };
    match f.keyword().unwrap_or("") {
        "type" => type_field(f, ord, p),
        "rec" => {
            generic_own(f)?;
            for t in &items[1..] {
                if t.keyword() == Some("type") {
                    type_field(t, ord, p)?;
                } else {
                    generic(t)?;
                }
            }
            Ok(())
        }
        "func" => {
            let i = next(&mut ord.funcs);
            func(f, i, p)
        }
        "import" => {
            generic_own(f)?;
            for d in &items[1..] {
                let (sub, c) = match d.keyword() {
                    Some("func") => (sub::FUNC, &mut ord.funcs),
                    Some("table") => (sub::TABLE, &mut ord.tables),
                    Some("memory") => (sub::MEMORY, &mut ord.memories),
                    Some("global") => (sub::GLOBAL, &mut ord.globals),
                    Some("tag") => (sub::TAG, &mut ord.tags),
                    _ => {
                        generic(d)?;
                        continue;
                    }
                };
                let i = next(c);
                named(p, sub, i, d)?;
                // An imported function has no locals, so a parameter's `@name` has nothing to
                // name — accepted, as wasm-tools accepts it, and dropped for the same reason.
                d.as_list().unwrap_or(&[]).iter().try_for_each(|c| {
                    if c.keyword() == Some("param") { own_name(c).map(|_| ()) } else { generic(c) }
                })?;
            }
            Ok(())
        }
        kw => {
            let c = match kw {
                "table" => Some((sub::TABLE, &mut ord.tables)),
                "memory" => Some((sub::MEMORY, &mut ord.memories)),
                "global" => Some((sub::GLOBAL, &mut ord.globals)),
                "tag" => Some((sub::TAG, &mut ord.tags)),
                "elem" => Some((sub::ELEM, &mut ord.elems)),
                "data" => Some((sub::DATA, &mut ord.datas)),
                _ => None,
            };
            match c {
                Some((sub, c)) => {
                    let i = next(c);
                    named(p, sub, i, f)?;
                }
                None => generic_own(f)?,
            }
            // The inline-segment abbreviations define a segment IN PLACE, so it takes the next
            // index of its space: `(memory (data "x"))` is a data segment, `(table funcref
            // (elem $f))` an element segment. Missing this would shift every later name.
            for c in items {
                match (kw, c.keyword()) {
                    ("memory", Some("data")) => ord.datas += 1,
                    ("table", Some("elem")) => ord.elems += 1,
                    _ => {}
                }
            }
            items.iter().try_for_each(generic)
        }
    }
}

fn type_field(t: &Sexpr, ord: &mut Ords, p: &mut Plan) -> Result<()> {
    let ti = next(&mut ord.types);
    named(p, sub::TYPE, ti, t)?;
    // ⚠️ A struct FIELD takes no `@name` — wasm-tools refuses `(field $x (@name "X") i32)` and
    // `(field (@name "X") i32)` alike, so everything below the type's own slot is generic. A
    // field's `$id` still reaches the name section; only the annotation form is refused.
    t.as_list().unwrap_or(&[]).iter().try_for_each(generic)
}

/// Header clauses of a function, which precede its body.
fn is_header(s: &Sexpr) -> bool {
    matches!(s.keyword(), Some("import" | "export" | "type" | "param" | "result" | "local"))
}

fn func(f: &Sexpr, fi: u32, p: &mut Plan) -> Result<()> {
    let items = f.as_list().unwrap_or(&[]);
    let mut k = name_slot(items);
    let mut decl = 0u32;
    let first_local = p.locals.len();
    while k < items.len() && is_header(&items[k]) {
        let c = &items[k];
        if matches!(c.keyword(), Some("param" | "local")) {
            let cl = c.as_list().unwrap_or(&[]);
            let n = if cl.get(1).is_some_and(is_id) { 1 } else { cl.len().saturating_sub(1) as u32 };
            if let Some(name) = own_name(c)? {
                p.locals.push((fi, decl, 0, name));
            }
            decl += n;
            cl.iter().try_for_each(generic)?;
        } else {
            generic(c)?;
        }
        k += 1;
    }
    for l in &mut p.locals[first_local..] {
        l.2 = decl;
    }
    // A function list holds three kinds of annotation position at once: its own name slot, its
    // header (where nothing may stand), and its body (hints, flat-form label names).
    let annots = f.annotations();
    check_hints(annots, items.len())?;
    let slot = name_slot(items);
    let mut named_once = false;
    for a in annots {
        match a.id.as_str() {
            CUSTOM => return Err(err("misplaced @custom annotation")),
            NAME if a.before == slot && !named_once => {
                named_once = true;
                p.names.push((sub::FUNC, fi, name_value(&a.body)?));
            }
            NAME if a.before > k && label_anchor(items, a.before).is_some() => {
                name_value(&a.body)?;
            }
            NAME => return Err(err("misplaced @name annotation")),
            _ if a.before < k => {
                return Err(err("@metadata.code.branch_hint annotation: must precede an instruction"));
            }
            _ => {}
        }
    }
    for c in &items[k..] {
        if let Some(ci) = c.as_list() {
            body(ci, c.annotations(), 0)?;
        }
    }
    Ok(())
}

/// An instruction sequence (or a folded instruction): hints and label `@name`s are legal here,
/// `@custom` never is.
fn body(items: &[Sexpr], annots: &[Annot], from: usize) -> Result<()> {
    check_hints(annots, items.len())?;
    for a in annots {
        match a.id.as_str() {
            CUSTOM => return Err(err("misplaced @custom annotation")),
            NAME if a.before >= from && label_anchor(items, a.before).is_some() => {
                name_value(&a.body)?;
            }
            NAME => return Err(err("misplaced @name annotation")),
            _ => {}
        }
    }
    for c in &items[from..] {
        if let Some(ci) = c.as_list() {
            body(ci, c.annotations(), 0)?;
        }
    }
    Ok(())
}

/// Branch hints and label names inside ONE function body, keyed by the address of the item
/// each applies to.
#[derive(Default)]
pub(super) struct BodyMarks {
    pub hints: BTreeMap<usize, u8>,
    pub labels: BTreeMap<usize, Vec<u8>>,
    /// How many hints the body holds. The emitter must record exactly this many: a hint it
    /// never reached (one before an immediate, say) would otherwise vanish without a word.
    pub expected_hints: usize,
}

pub(super) fn addr(s: &Sexpr) -> usize {
    core::ptr::from_ref(s) as usize
}

/// Collect the marks of a function body. `annots` are the body's own, rebased so that
/// `before` indexes `body`. Positions were validated by [`plan`], so this only records.
pub(super) fn body_marks(body: &[Sexpr], annots: &[Annot]) -> BodyMarks {
    fn fill(items: &[Sexpr], annots: &[Annot], m: &mut BodyMarks) {
        for a in annots {
            if a.id == HINT {
                if let (Some(it), Ok(v)) = (items.get(a.before), hint_value(&a.body)) {
                    m.hints.insert(addr(it), v);
                    m.expected_hints += 1;
                }
            } else if a.id == NAME {
                if let (Some(at), Ok(n)) = (label_anchor(items, a.before), name_value(&a.body)) {
                    m.labels.insert(addr(&items[at]), n);
                }
            }
        }
        for c in items {
            if let Some(ci) = c.as_list() {
                fill(ci, c.annotations(), m);
            }
        }
    }
    let mut m = BodyMarks::default();
    fill(body, annots, &mut m);
    m
}

/// Where a section of id `id` sits among the placement anchors; `None` for the data-count
/// section, which has no anchor of its own and lies between `elem` and `code`.
fn anchor_of(id: u8) -> Option<usize> {
    Some(match id {
        1 => A_TYPE,
        2 => A_IMPORT,
        3 => A_FUNC,
        4 => A_TABLE,
        5 => A_MEMORY,
        13 => A_TAG,
        6 => A_GLOBAL,
        7 => A_EXPORT,
        8 => A_START,
        9 => A_ELEM,
        10 => A_CODE,
        11 => A_DATA,
        _ => return None,
    })
}

fn read_uleb(b: &[u8], pos: &mut usize) -> Option<usize> {
    let (mut v, mut shift) = (0usize, 0u32);
    loop {
        let byte = *b.get(*pos)?;
        *pos += 1;
        v |= usize::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
    }
}

fn push_custom(out: &mut Vec<u8>, name: &[u8], data: &[u8]) {
    let mut c = Vec::new();
    super::uleb(&mut c, name.len() as u64);
    c.extend_from_slice(name);
    c.extend_from_slice(data);
    out.push(0);
    super::uleb(out, c.len() as u64);
    out.extend_from_slice(&c);
}

/// Lay the custom sections into a finished module: every `@custom` at its slot, the branch-hint
/// section immediately before `code` (where wasm-tools puts it), and the `name` section last.
///
/// Done as one pass over the assembler's OWN output rather than at each of the fourteen section
/// writers, so that no writer can forget to offer its slot — the failure mode of a rule applied
/// at every site instead of once.
pub(super) fn lay_out(
    module: &[u8],
    customs: &[CustomSec],
    hints: Option<&[u8]>,
    names: Option<&[u8]>,
) -> Vec<u8> {
    let mut order: Vec<usize> = (0..customs.len()).collect();
    order.sort_by_key(|&i| customs[i].slot); // stable: source order within a slot
    let mut next = 0;
    let mut out = module[..8.min(module.len())].to_vec();
    let mut flush = |out: &mut Vec<u8>, upto: u8| {
        while next < order.len() && customs[order[next]].slot <= upto {
            let c = &customs[order[next]];
            push_custom(out, &c.name, &c.data);
            next += 1;
        }
    };
    let mut pos = 8;
    while pos < module.len() {
        let start = pos;
        let id = module[pos];
        pos += 1;
        let Some(len) = read_uleb(module, &mut pos) else { break };
        let end = (pos + len).min(module.len());
        match anchor_of(id) {
            Some(a) => flush(&mut out, slot_before(a)),
            None => flush(&mut out, slot_after(A_ELEM)),
        }
        if id == 10 {
            if let Some(h) = hints {
                push_custom(&mut out, super::BRANCH_HINT_SECTION, h);
            }
        }
        out.extend_from_slice(&module[start..end]);
        if let Some(a) = anchor_of(id) {
            flush(&mut out, slot_after(a));
        }
        pos = end;
    }
    flush(&mut out, SLOT_LAST);
    if let Some(n) = names {
        push_custom(&mut out, b"name", n);
    }
    out
}
