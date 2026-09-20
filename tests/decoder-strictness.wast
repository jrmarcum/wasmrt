;; DECODER STRICTNESS — the bytes a decoder must refuse, and two it must NOT.
;;
;; Regression file for eight decoder defects found and fixed 2026-09-19. Every module below was
;; run through wasm-tools 1.259 (`wasm-tools validate --features all`) BEFORE the fix and after:
;; the six `assert_malformed` cases are bytes wasmrt accepted and wasm-tools refused, and the two
;; `module binary` cases at the end are VALID modules wasm-tools accepts that wasmrt refused.
;;
;; ⚠️ Message text is not compared by this runner; the strings quote wasm-tools' wording so the
;; verdict this file pins can be re-checked against it by hand.

;; --- 1. an unassigned SIMD sub-opcode is not an instruction -------------------------------
;; `sub <= MAX_SIMD_SUB` is a CEILING, not membership: the 0xFD space has 20 holes below its top,
;; and all 20 decoded. Swept against wasm-tools over the whole space 0x00..=0x113.
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\07\01\05\00\fd\9a\01\0b") "unknown 0xfd subopcode: 0x9a")
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\07\01\05\00\fd\ee\01\0b") "unknown 0xfd subopcode: 0xee")

;; --- 2. atomic.fence's operand is a RESERVED byte, not a memory index ---------------------
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\07\01\05\00\fe\03\01\0b") "nonzero byte after `atomic.fence`")

;; --- 3. br_on_cast flags: only bits 0 and 1 are defined -----------------------------------
;; The byte was read for its two bits and the rest discarded, so `br_on_cast 0xff` ran as 0x03.
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\10\01\0e\00\02\6e\d0\6e\fb\18\07\00\6e\6e\0b\1a\0b") "invalid cast flags: 00000111")
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\10\01\0e\00\02\6e\d0\6e\fb\18\ff\00\6e\6e\0b\1a\0b") "invalid cast flags: 11111111")
;; …and the same module with DEFINED flags still validates — a rejection test that cannot tell
;; the two apart is not testing the flags.
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\10\01\0e\00\02\6e\d0\6e\fb\18\03\00\6e\6e\0b\1a\0b")

;; --- 4. element segment flags: §5.5.12 defines eight forms, so the field is three bits ----
;; flags = 8 was decoded as flags = 0, i.e. as a DIFFERENT segment form, rather than refused.
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\04\04\01\70\00\01\09\07\01\08\41\00\0b\00\00\0a\04\01\02\00\0b") "invalid flags byte in element segment")

;; --- 5. elemkind: 0x00 (funcref) is the only one defined ---------------------------------
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\04\04\01\70\00\01\09\04\01\01\01\00\0a\04\01\02\00\0b") "only the function external type is supported in elem segment")

;; --- 6. array.new_data / array.init_data require the data-count section -------------------
;; The requirement listed `memory.init` and `data.drop` only: it was written before GC added two
;; more instructions that name a data segment. ⚠️ wasmrt's own ASSEMBLER had the same gap, so
;; `wasmrt wat` emitted modules wasm-tools refused — the T10a emitter mechanism again.
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\07\02\5e\78\01\60\00\00\03\02\01\01\0a\0d\01\0b\00\41\00\41\00\fb\09\00\00\1a\0b\0b\03\01\01\00") "data count section required")

;; --- 7. two VALID modules a hand-written const-expr skipper could not read ----------------
;; `module.rs` carried a second, partial copy of the instruction grammar whose only job was to
;; find an init expression's `end`. It did not know `struct.new_desc` (custom-descriptors) and
;; read `ref.null`'s heap type with a bare s33, missing the `exact` prefix `0x62 typeidx`. With
;; type index 11 the stranded byte IS `0x0b` = `end`, so the expression ended early and the
;; global section came up short: `decode failed: section size mismatch` on a module wasm-tools
;; accepts. Both are gone structurally — const exprs now go through `opcode::decode_expr`.
;; (⚠️ wasmtime 48 has no custom-descriptors; wasm-tools 1.259 is the outside reader here.)
(module binary "\00\61\73\6d\01\00\00\00\01\2c\0c\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\4e\02\4d\0c\5f\00\4c\0b\5f\00\06\0b\01\63\0b\00\fb\01\0c\fb\20\0b\0b")
(module binary "\00\61\73\6d\01\00\00\00\01\2c\0c\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\60\00\00\4e\02\4d\0c\5f\00\4c\0b\5f\00\06\09\01\63\62\0b\00\d0\62\0b\0b")

;; --- 8. a table type's element type is a REFERENCE type (§5.3.9) --------------------------
;; `tabletype ::= reftype limits`, and `read_table_type` read a VALUE type. So a table of
;; `i64` (`\7e`) or `v128` (`\7b`) decoded, validated and RAN: `table.get` handed the guest the
;; engine's null sentinel as a number (measured: -1, and 0xffffffffffffffff for the v128 form).
;; wasm-tools and wasmtime 48: "malformed reference type".
;;
;; 🎓 The element SEGMENT's type field (case 5 above, and the test in `module.rs`) is the SAME
;; grammar production and was fixed on its own, because that is the one the suite complained
;; about. Neither the spec suite nor the `.wat` corpus can reach this one — the text format has
;; no way to spell it, so only a hand-built binary or a differential fuzz finds it. A
;; wasm-tools-smith fuzz found it in 215 modules.
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\04\04\01\7e\00\01\07\05\01\01\66\00\00\0a\09\01\07\00\41\00\25\00\1a\0b") "malformed reference type")
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\04\04\01\7b\00\01\07\05\01\01\66\00\00\0a\09\01\07\00\41\00\25\00\1a\0b") "malformed reference type")
;; …and the identical module with `funcref` must still decode, validate and run, so this is a
;; test of the FIELD and not of the surrounding bytes.
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\04\04\01\70\00\01\07\05\01\01\66\00\00\0a\09\01\07\00\41\00\25\00\1a\0b")
