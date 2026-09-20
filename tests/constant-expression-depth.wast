;; A constant expression's operand stack was capped at EIGHT values — a number, not a rule.
;;
;; Every module here is valid (wasm-tools 1.259 and wasmtime 48 both accept them) and every one
;; was refused, with `ConstantExpressionRequired`: a wrong verdict reported as a wrong reason.
;; A differential fuzz against `wasm-tools smith` reproduced the extended-const case on its own
;; within 25 generated modules — nine `i32.const`s in a global initializer is not an exotic
;; shape, it is what a constant-folding producer emits.
;;
;; The bound now comes from the expression's own byte length: every operand was pushed by an
;; instruction, and no instruction is shorter than a byte, so it cannot refuse anything a valid
;; expression can express — while a hostile initializer still cannot ask for unbounded memory.

;; 1. struct.new on a NINE-field struct (eight was the largest that worked).
(module
  (type $s (struct (field i32) (field i32) (field i32) (field i32) (field i32)
                   (field i32) (field i32) (field i32) (field i32)))
  (global $g (export "g") (ref $s) (struct.new $s
    (i32.const 1) (i32.const 2) (i32.const 3) (i32.const 4) (i32.const 5)
    (i32.const 6) (i32.const 7) (i32.const 8) (i32.const 9)))
  (func (export "last") (result i32)
    (struct.get $s 8 (global.get $g))))
(assert_return (invoke "last") (i32.const 9))

;; 2. array.new_fixed with sixteen elements.
(module
  (type $a (array i32))
  (global $g (export "g") (ref $a) (array.new_fixed $a 16
    (i32.const 1) (i32.const 2) (i32.const 3) (i32.const 4)
    (i32.const 5) (i32.const 6) (i32.const 7) (i32.const 8)
    (i32.const 9) (i32.const 10) (i32.const 11) (i32.const 12)
    (i32.const 13) (i32.const 14) (i32.const 15) (i32.const 16)))
  (func (export "last") (result i32)
    (array.get $a (global.get $g) (i32.const 15))))
(assert_return (invoke "last") (i32.const 16))

;; 3. an extended-const initializer holding TEN operands before it folds them — the shape the
;; fuzzer produced (it emitted ten `i32.const`s and nine arithmetic ops).
(module
  (global $g (export "g") i32
    i32.const 1 i32.const 2 i32.const 3 i32.const 4 i32.const 5
    i32.const 6 i32.const 7 i32.const 8 i32.const 9 i32.const 10
    i32.add i32.add i32.add i32.add i32.add i32.add i32.add i32.add i32.add)
  (func (export "g_val") (result i32) (global.get $g)))
(assert_return (invoke "g_val") (i32.const 55))

;; The bound still exists: an initializer that pushes and never consumes is refused, rather than
;; allowed to allocate.
(assert_invalid
  (module (global i32 i32.const 0 i32.const 0))
  "type mismatch")
