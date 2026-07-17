# wazmrt decode + IR core — Port Map (types/Reader/Module/opcode)

## 0. Cross-cutting
- Reader is a value type `struct{bytes:[]const u8, pos:usize}`, COPIED to fork cursors (`var scan=r.*`) — load-bearing in type-section pre-scan. Rust: `struct Reader<'a>{bytes:&'a[u8],pos:usize}` Copy; `let mut scan=r;`.
- Zero-copy vs owned: Section{offset,size} = metadata only (don't index input after decode). Everything Module exposes as a slice is ARENA-OWNED copies (names, bodies, const-exprs, data @memcpy'd) so input may be freed. body/offset_expr INCLUDE terminating `end` byte.
- Error taxonomy: one flat DecodeError ∪ Allocator.Error = Module.Error. Rust: alloc infallible → Module::Error collapses to DecodeError.
- Non-exhaustive enums (`_`) on SectionId/ValType/ExternKind/Op — unknown byte → enum value a later check rejects, not a decoder trap.

## 1. types.zig
magic=[0,0x61,0x73,0x6d]; supported_version:u32=1 (only 1 accepted).
SectionId enum(u8): custom0 type1 import2 function3 table4 memory5 global6 export7 start8 element9 code10 data11 data_count12, `_`. max=12; raw>max → InvalidSectionId.
ExternKind enum(u8): func0 table1 memory2 global3, `_`. BINARY order, DIFFERS from wasm-c-api wasm_externkind_t order — C ABI remaps.

### ValType : enum(u32) — bit-packed (CRITICAL)
Numeric/abstract keep real 1-byte wire value (<0x100): i32=0x7f i64=0x7e f32=0x7d f64=0x7c v128=0x7b; funcref=0x70 externref=0x6f anyref=0x6e eqref=0x6d i31ref=0x6c structref=0x6b arrayref=0x6a nullref=0x71.
Non-nullable synthetic tags (internal, unused valtype-byte range; external 0x64 ht maps here): funcref_nn=0x68 externref_nn=0x67 anyref_nn=0x66 eqref_nn=0x65 i31ref_nn=0x62 structref_nn=0x61 arrayref_nn=0x59 nullref_nn=0x58. Plus `_`.
Concrete typed-ref packing (high bits of u32 — MOST load-bearing layout):
  concrete_bit=0x8000_0000 (bit31); nullable_bit=0x4000_0000 (bit30); kind_shift=28; kind_mask=0x3<<28 (bits28-29 family); index_mask=0x0fff_ffff (bits0-27 type idx).
  Family kind codes bits28-29: func=0 struct=1 array=2 (3 unreachable). Only func/struct/array become concrete.
Methods (pure bit ops, port verbatim): concreteRef(is_nullable,kind:RefHeap,ti)= concrete_bit | (nullable?nullable_bit) | (k<<28) | (ti&index_mask), asserts kind∈{func,struct,array}. isConcrete/concreteIndex/isValid/isRef/isNonNullRef/nullable()/refHeap().

### RefHeap enum (ordinary tag): func extern_ any eq i31 struct array none.
valType(is_nullable)→collapsed nullable/_nn ValType. top(): func→func, extern_→extern_, else→any.
sub(a,b) abstract subtyping lattice (spec-critical): a==b true; none <: {i31,struct,array,eq,any}; {i31,struct,array} <: {eq,any}; eq <: any; func/extern/any disjoint no proper supertype.

### DecodeError set: UnexpectedEof BadMagic UnsupportedVersion LebOverflow InvalidSectionId BadFuncType BadType UnknownExternKind IndexOutOfRange MalformedFlag BadValType DataCountMismatch UnsupportedOpcode. (composite non-func-form → BadType not BadFuncType.) UnsupportedOpcode doubles as "unimplemented feature" (SIMD 0xFD, atomics, EH, data-seg flag≥3).

## 2. Reader.zig — zero-copy + spec-correct LEB128
Fields bytes/pos. init/remaining/atEnd. readByte (EOF-checked, defer post-inc); peekByte; readBytes(n) borrowed subslice, remaining<n→UnexpectedEof. readU32Le/readF32Bits (4B LE→u32); readF64Bits (8B LE→u64). Floats carried as raw bits, never parsed.
readVarU32: 7 bits/byte; at shift==28 (5th byte) only 4 value bits — byte>>4 != 0 → LebOverflow, return. 6-byte 0x80.. case: 5th byte 0x80 → 0x80>>4=8≠0 → LebOverflow.
readVarI32: at shift==28 byte&0x80→LebOverflow; hi=byte&0x78 must be all-0 or all-1 (sign-ext of bit31) else LebOverflow; terminator byte&0x40 → sign-extend result|=~0<<(shift+7); bitCast i32.
readVarI64: u64 accum shift:u6; at shift==63 (10th byte) byte&0x80→LebOverflow; v=byte&0x7f must be 0x00 or 0x7f else LebOverflow. Used for s33 block/heap types (semantic range checked by caller).
skipLeb(max_bytes): consume until continuation clears; n>=max before clear→LebOverflow. Callers pass 5 or 10.
PORT: transcribe LEB decoders bit-for-bit — the over-long/too-large rejection is exactly what conformance probes.

## 3. Module.zig — data model + decode()
Module fields: arena (owns all), version:u32, sections:[]Section, comp_types:[]CompType, supertypes:[]?u32, functions:[]u32 (type idx/defined func), imports:[]Import, exports:[]Export, code:[]Code, globals:[]GlobalType (imported-then-defined), global_inits:[]([]u8) (defined, tail-aligned), memories, tables, data:[]DataSegment, elements:[]Element, start:?u32, func_names:?[]u8 (raw name subsection, lazily scanned).
Nested: Section{id,offset,size} metadata only. FuncType{params,results}. StorageType=union{val:V,i8,i16}, .unpacked()→i32, .isPacked(). FieldType{storage,mutable}. CompKind{func,struct,array}. CompType=union{func:FuncType,struct:[]FieldType,array:FieldType}. Limits{min:u32,max:?u32}. TableType{element:V,limits}. MemoryType{limits}. GlobalType{content:V,mutable}. Extern=union{func,table,memory,global}. Import{module,name,type:Extern}. Export{name,index,type:Extern}. DataSegment{active,mem_index,offset_expr,bytes}. Element{mode(active/passive/declarative),table_index,offset_expr,funcs:[]u32,exprs:[][]u8,elem_type:V} (exactly one funcs/exprs). Code{locals:[]Local,body:[]u8 (incl end),body_offset:u32 (abs offset in original binary for truthful frame_module_offset)}.
decode() order: readBytes(4)==magic else BadMagic; readU32Le==1 else UnsupportedVersion. Loop until atEnd: raw_id byte (>max→InvalidSectionId), size=readVarU32, offset=r.pos, payload=readBytes(size) borrow, append Section; sub=Reader.init(payload) dispatch by id (each section in own sub-reader so malformed inner vec can't run past boundary). data_count→readVarU32; start→readVarU32. Post-loop: data_count present && !=data.len → DataCountMismatch. errdefer arena.deinit().
Custom/name: name=="name" → findFuncNameSubsection (errors degrade to null, never fails module); kind==1 arena-dup, kind>1 break. funcName(idx) lazy linear scan, any parse error→null.
Type section — rec/sub/composite + FORWARD-REF PRE-SCAN (two mirrored passes, cursor copied):
  Pass A prescanTypeKinds: walk vec(rectype); 0x4e rec-group (count) or bare sub. scanSubType: optional 0x50(non-final)/0x4f(final) + supertype indices (skipped), composite byte 0x60/0x5f/0x5e → append .func/.struct/.array to kinds, skip body. Output type_kinds gives every idx its family BEFORE bodies read.
  Pass B decodeTypeSection: real decode. decodeSubType: 0x50/0x4f wrapper ns=readVarU32; ns>1→BadType (GC MVP ≤1 super); ns==1 super=readVarU32. decodeCompType: 0x60 func / 0x5f struct(n fields) / 0x5e array(1 field) else BadType. Rec groups flatten into consecutive indices.
  WHY pre-scan: readValType/readHeapTypeRef need kinds[ti] to collapse (ref $t) to concrete family head, and $t may forward-ref later type in same rec group. Port MUST replicate two-pass (or forward-decl pass).
readValType(r,kinds): numerics self-map; 0x70/0x73→funcref; 0x6f/0x72→externref; 0x6e..0x6a,0x71→any-family; 0x69/0x74→externref (exnref opaque); 0x68..0x58→_nn synthetic; 0x63→readHeapTypeRef(nullable=true); 0x64→readHeapTypeRef(nullable=false); else BadValType.
readHeapTypeRef(r,nullable,kinds): ht=readVarI64() s33. ht>=0 concrete idx (ti>=kinds.len→IndexOutOfRange; map kinds[ti] family→concreteRef). Negative abstract: -0x10/-0x0d func/nofunc→funcref, -0x11/-0x0e extern, -0x12 any, -0x13 eq, -0x14 i31, -0x15 struct, -0x16 array, -0x0f none; else externref.
Other readers: readName (copied to arena); readLimits (flag>0x01→MalformedFlag; min; max iff flag&1); readGlobalType (content + mut byte >0x01→MalformedFlag); readFieldType (storage 0x78→i8/0x77→i16/else valtype + mut byte); skipConstExpr (until 0x0b; special-case operand ops: 0x41/0x23/0xd2→skipLeb5, 0x42→skipLeb10, 0x43→4B, 0x44→8B, 0xd0→readVarI64); readConstExprBytes (raw span incl end, copy arena).
Import section: name,name,kind→resolve Extern AND append to index space (func via funcTypeAt=comp_types[ti] must be .func). Unknown kind→UnknownExternKind. Function section: vec type indices→func_space. Table/Memory/Global→spaces; globals push readConstExprBytes init. Export: name,kind,index→spaceAt (index>=len→IndexOutOfRange). Element (8 flag variants): flags=readVarU32; bit0 active/passive; bit1 declarative-or-explicit-table; bit2 const-expr form; defaults table_index=0 elem_type=funcref. Data: flags 0 active mem0 / 1 passive / 2 active explicit mem / else→UnsupportedOpcode. Code(payload_base): each entry readBytes(readVarU32) sub-reader, decodeLocals then body=copy(rest incl end), body_offset=payload_base+(r.pos-entry.len)+er.pos.
Query helpers: section(id), importedFunc/Table/MemoryCount, funcType(index) (imports-first walk then defined), funcTypeIndex, funcSig(ti), structFields(ti), arrayField(ti) (null on OOB/wrong kind), refHead(HeapType), isSubtype(a,b) (walk declared supertypes chain, reflexive/transitive).

## 4. opcode.zig — shared authority (validate + interp + assembler-in-reverse)
Op enum(u8): real MVP ops use TRUE wire byte 0x00-0xC4; call_ref=0x14 return_call_ref=0x15; ref_null=0xd0 ref_is_null=0xd1 ref_func=0xd2 ref_eq=0xd3 ref_as_non_null=0xd4 br_on_null=0xd5 br_on_non_null=0xd6.
MULTI-BYTE-PREFIX ops get SYNTHETIC internal u8 tags (NOT wire byte): sat-trunc(0xFC 0x00-07)=0xc5-0xcc; bulk mem(0xFC 0x08-0b) memory_init=0xd7 data_drop=0xd8 memory_copy=0xd9 memory_fill=0xda; table ops(0xFC 0x0c-11) table_init=0xe0..table_fill=0xe5; GC array(0xFB) array_new=0xe6..array_len=0xed; GC struct/i31/cast(0xFB) ref_i31=0xf0 i31_get_s=0xf1 i31_get_u=0xf2 struct_new=0xf3..struct_set=0xf8 ref_test=0xee ref_cast=0xef br_on_cast=0xf9 br_on_cast_fail=0xfa.
fcSubOpcode(op)→?u8 / gcSubOpcode(op)→?u8 = exact tag→wire-sub reverse maps (0xFC/0xFB). ref_test/ref_cast map to NON-null sub (0x14/0x16); null form (0x15/0x17) chosen at emit from RefType.nullable.
IR: HeapType=union{func,extern_,any,eq,i31,struct,array,none,nofunc,noextern,concrete:u32}. RefType{nullable,heap}. BlockType=union{empty,value:V,type_index:u32}. MemArg{alignment:u32,offset:u32}. BrTable{labels,default}. CallIndirect{type_index,table}. Imm=union(17+): none block_type label:u32 br_table func:u32 call_indirect local global table elem data table_init{elem,table} table_copy{dst,src} mem:MemArg mem_reserved:u8 i32 i64 f32:u32 f64:u64 select_types:[]V ref_type:HeapType gc_type:u32 gc_field{type_index,field} gc_type_n{type_index,n} ref_cast:RefType br_cast{label,src,dst}. Instr{op,imm}. ImmKind (adds data_init,mem_copy internal-only, unsupported).
immediateKind(op)→ImmKind: big switch on @intFromEnum. (see decode detail — blocks 0x02-04 block_type; 0x0c/0d,0xd5,d6 label; 0x0e br_table; 0x10,14,15,d2 func; 0x11 call_indirect; 0x20-22 local; 0x23,24 global; 0x25,26,e3-e5 table; 0xe0 table_init; 0xe1 elem; 0xe2 table_copy; 0xd7 data_init; 0xd8 data; 0xd9 mem_copy; 0xda mem_reserved; 0x28-3e mem; 0x3f,40 mem_reserved; 0x41-44 consts; 0x1c select_types; 0xd0 ref_type; GC gc_type/gc_field/gc_type_n; casts ref_cast/br_cast; else unsupported.)
readBlockType (s33): v>=0 type_index; -64 empty; -1..-5 i32/i64/f32/f64/v128; -16..-22,-15 abstract ref valtypes; -24..-27,-30,-31,-39,-40 _nn synthetic; else UnsupportedOpcode.
readHeapType (s33): v>=0 concrete:ti; -0x10..-0x16,-0x0f,-0x0d,-0x0e func/extern/any/eq/i31/struct/array/none/nofunc/noextern; else UnsupportedOpcode.
decodeBody = decodeBodyTracked(...,null). Tracked variant appends each instr byte-offset to parallel ArrayList(u32) (kept OUT of Instr; consumed only by trap reports).
  0xfb prefix: readVarU32 sub → build Instr with internal Op tag. 0x00/01 struct_new(_default); 0x02-05 struct_get*/set (readGcField type_index+field); 0x06/07 array_new(_default); 0x08 array_new_fixed (gc_type_n); 0x0b-0e array_get*/set; 0x0f array_len; 0x14/15 ref_test (ref_cast{nullable=false/true,heap=readHeapType}); 0x16/17 ref_cast; 0x18/19 br_on_cast(_fail) readBrCast; 0x1c-1e ref_i31/i31_get_s/u; else UnsupportedOpcode.
  0xfc prefix: readVarU32 sub → 0x00-07 sat-trunc; 0x08 memory_init (data idx + discard reserved mem byte); 0x09 data_drop; 0x0a memory_copy (discard 2 reserved); 0x0b memory_fill (mem_reserved byte); 0x0c-11 table_init/elem_drop/table_copy/table_grow/size/fill; else UnsupportedOpcode.
  else: op=@enumFromInt(b0); imm by immediateKind. select_types = count + n reftype BYTES (@enumFromInt(readByte), NOT LEB). Internal-tag kinds + unsupported in non-prefixed path → UnsupportedOpcode (raw synthetic tag in stream is malformed).
readBrCast: flags byte (bit0 src nullable, bit1 dst nullable), label LEB, src & dst heap types → br_cast{label,src,dst}.
Scope: 0xFD SIMD, atomics, EH → UnsupportedOpcode. 0xFC + 0xFB fully implemented.

## Port gotchas
1. ValType u32 bit-packing — carry as u32 newtype, NOT plain enum (concrete refs are a value range).
2. Two-pass type-section decode with copied cursor — required for rec-group forward refs.
3. LEB over-long/too-large rejection — transcribe 5th-byte (>>4, sign bits) + 10th-byte (v∈{0,0x7f}) exactly.
4. Synthetic Op tags ≠ wire bytes — fc/gcSubOpcode reverse maps are emit-side truth; keep enum values stable.
5. body/const-expr slices include `end` byte; owned via arena.
6. Reserved bytes (bulk-op mem indices, size/grow) read-and-discarded, not validated to 0.
7. Name/unknown sections never fail decode; unknown data-seg flag DOES; data_count mismatch fails post-loop.
8. select_types reftypes single bytes, no LEB, no concrete-ref resolution at decode.
