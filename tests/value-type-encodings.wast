;; A VALUE TYPE inside a function body — a block result and a typed `select` result — read through ONE
;; reader (`opcode::read_value_type_from`). Regression test for three defects fixed 2026-09-19, each measured
;; against wasm-tools over every one-byte encoding 0x40..0x7f (and this file checked on wasmtime 48):
;;
;;   1. INTERNAL TAGS ACCEPTED AS WIRE BYTES — wasmrt's `ValType` gives the non-null abstract references
;;      internal one-byte tags, and both readers accepted them: `0x66` read as `(ref any)`. No binary spells a
;;      non-null reference in one byte; §5.3.5 has only `0x64 ht`. (20 encodings.)
;;   2. VALID SHORTHANDS REFUSED — the block-type table never listed the hierarchy bottoms `0x72`/`0x73`/`0x74`
;;      (nullexternref / nullfuncref / nullexnref), so a valid block type was refused.
;;   3. THE LONG FORM UNREADABLE IN `select` — `select (result (ref $t))` (`0x63`/`0x64` + heap type) could not be
;;      decoded at all, so every typed select over a non-nullable or concrete reference was refused.
;;
;; `0x68`/`0x75` are stack-switching's `cont`/`nocont`: refused, as the standard (no stack switching) refuses them.

;; --- 1. internal tags are not value-type bytes (messages: wasmtime 48 wording) ---
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\54\1a\0b") "invalid value type") ;; select 0x54
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\55\1a\0b") "invalid value type") ;; select 0x55
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\56\1a\0b") "invalid value type") ;; select 0x56
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\57\1a\0b") "invalid value type") ;; select 0x57
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\58\1a\0b") "invalid value type") ;; select 0x58
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\59\1a\0b") "invalid value type") ;; select 0x59
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\61\1a\0b") "invalid value type") ;; select 0x61
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\62\1a\0b") "unexpected exact type") ;; select 0x62
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\65\1a\0b") "invalid value type") ;; select 0x65
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\66\1a\0b") "invalid value type") ;; select 0x66
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\67\1a\0b") "invalid value type") ;; select 0x67
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\68\1a\0b") "continuation refs not supported") ;; select 0x68
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\75\1a\0b") "continuation refs not supported") ;; select 0x75
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\57\00\0b\1a\0b") "invalid value type") ;; block 0x57
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\58\00\0b\1a\0b") "invalid value type") ;; block 0x58
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\59\00\0b\1a\0b") "invalid value type") ;; block 0x59
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\61\00\0b\1a\0b") "invalid value type") ;; block 0x61
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\62\00\0b\1a\0b") "unexpected exact type") ;; block 0x62
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\65\00\0b\1a\0b") "invalid value type") ;; block 0x65
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\66\00\0b\1a\0b") "invalid value type") ;; block 0x66
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\67\00\0b\1a\0b") "invalid value type") ;; block 0x67
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\68\00\0b\1a\0b") "continuation refs not supported") ;; block 0x68
(assert_malformed (module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\75\00\0b\1a\0b") "continuation refs not supported") ;; block 0x75

;; --- 2. every valid one-byte value type is a valid block type AND a valid select type ---
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\7f\00\0b\1a\0b") ;; block 0x7f
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\7f\1a\0b") ;; select 0x7f
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\7e\00\0b\1a\0b") ;; block 0x7e
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\7e\1a\0b") ;; select 0x7e
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\7d\00\0b\1a\0b") ;; block 0x7d
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\7d\1a\0b") ;; select 0x7d
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\7c\00\0b\1a\0b") ;; block 0x7c
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\7c\1a\0b") ;; select 0x7c
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\7b\00\0b\1a\0b") ;; block 0x7b
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\7b\1a\0b") ;; select 0x7b
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\74\00\0b\1a\0b") ;; block 0x74
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\74\1a\0b") ;; select 0x74
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\73\00\0b\1a\0b") ;; block 0x73
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\73\1a\0b") ;; select 0x73
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\72\00\0b\1a\0b") ;; block 0x72
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\72\1a\0b") ;; select 0x72
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\71\00\0b\1a\0b") ;; block 0x71
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\71\1a\0b") ;; select 0x71
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\70\00\0b\1a\0b") ;; block 0x70
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\70\1a\0b") ;; select 0x70
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\6f\00\0b\1a\0b") ;; block 0x6f
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\6f\1a\0b") ;; select 0x6f
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\6e\00\0b\1a\0b") ;; block 0x6e
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\6e\1a\0b") ;; select 0x6e
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\6d\00\0b\1a\0b") ;; block 0x6d
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\6d\1a\0b") ;; select 0x6d
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\6c\00\0b\1a\0b") ;; block 0x6c
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\6c\1a\0b") ;; select 0x6c
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\6b\00\0b\1a\0b") ;; block 0x6b
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\6b\1a\0b") ;; select 0x6b
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\6a\00\0b\1a\0b") ;; block 0x6a
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\6a\1a\0b") ;; select 0x6a
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\02\69\00\0b\1a\0b") ;; block 0x69
(module binary "\00\61\73\6d\01\00\00\00\01\04\01\60\00\00\03\02\01\00\0a\09\01\07\00\00\1c\01\69\1a\0b") ;; select 0x69

;; --- 3. the long form, in select and block, and it RUNS ---
(module
  (type $s (sub (struct (field i32))))
  (type $t (sub $s (struct (field i32))))
  (func (export "pick") (param i32) (result i32)
    (struct.get $s 0
      (select (result (ref $s))
        (struct.new $s (i32.const 1)) (struct.new $t (i32.const 2)) (local.get 0))))
  (func (export "pick-null") (param i32) (result i32)
    (ref.is_null
      (select (result (ref null $s)) (ref.null $s) (struct.new $s (i32.const 3)) (local.get 0))))
  (func (export "pick-any") (param i32) (result i32)
    (ref.test (ref $t)
      (select (result (ref any)) (struct.new $s (i32.const 4)) (struct.new $t (i32.const 5)) (local.get 0))))
  (func (export "block-nonnull") (result i32)
    (struct.get $s 0 (block (result (ref $s)) (struct.new $t (i32.const 6)))))
  ;; Not invoked: VALIDITY is the assertion — a non-null `select` result must flow into a non-null return.
  (func (export "nonnull-flows") (param i32) (result (ref $s))
    (select (result (ref $s)) (struct.new $s (i32.const 7)) (struct.new $t (i32.const 8)) (local.get 0)))
  (func (export "block-nullfunc") (result i32)
    (ref.is_null (block (result nullfuncref) (ref.null nofunc))))
)
(assert_return (invoke "pick" (i32.const 1)) (i32.const 1))
(assert_return (invoke "pick" (i32.const 0)) (i32.const 2))
(assert_return (invoke "pick-null" (i32.const 1)) (i32.const 1))
(assert_return (invoke "pick-null" (i32.const 0)) (i32.const 0))
(assert_return (invoke "pick-any" (i32.const 1)) (i32.const 0))
(assert_return (invoke "pick-any" (i32.const 0)) (i32.const 1))
(assert_return (invoke "block-nonnull") (i32.const 6))
(assert_return (invoke "block-nullfunc") (i32.const 1))

;; --- ...and the long form is TYPED, not merely decoded ---
(assert_invalid
  (module (type $s (sub (struct))) (type $t (sub $s (struct)))
    (func (param (ref $s)) (result (ref $t))
      (select (result (ref $t)) (local.get 0) (local.get 0) (i32.const 1))))
  "type mismatch")
(assert_invalid
  (module (type $s (struct))
    ;; The FUNCTION result is nullable, so the only thing wrong is `select`'s own operand typing.
    (func (param (ref null $s)) (result (ref null $s))
      (select (result (ref $s)) (local.get 0) (local.get 0) (i32.const 1))))
  "type mismatch")
(assert_invalid
  (module (type $s (struct))
    (func (param anyref) (result (ref null $s))
      (select (result (ref null $s)) (local.get 0) (local.get 0) (i32.const 1))))
  "type mismatch")
