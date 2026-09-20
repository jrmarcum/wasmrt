;; 64-bit tables: every index and count operand is the TABLE's index type — at validation AND
;; at run time.
;;
;; Two defects, found together by sweeping the whole 64-bit axis (24 instructions × 2 spellings)
;; against wasm-tools and wasmtime 48 rather than by reading the arms:
;;
;;   * `return_call_indirect` typed its index `i32` outright, so on a 64-bit table it REFUSED the
;;     valid i64 spelling and ACCEPTED an i32 one — both directions of one hardcoded type. It was
;;     the only arm of the 24 left behind when table64 landed, and no spec-suite file tail-calls
;;     through a 64-bit table.
;;
;;   * The INTERPRETER popped `i32` for every table operand, so a 64-bit index was truncated to
;;     32 bits: `call_indirect` at 2^32 called the function in slot **0** and returned its answer
;;     instead of trapping — a call to a function the guest never named. wasmtime traps:
;;     "undefined element: out of bounds table access". `Table::is64` was already recorded (for
;;     import matching) and the execution path had never read it.

(module
  (type $t (func (result i32)))
  (table $tb i64 2 funcref)
  (elem (table $tb) (i64.const 0) func $zero $one)
  (func $zero (type $t) (i32.const 100))
  (func $one (type $t) (i32.const 101))

  (func (export "call") (param i64) (result i32)
    (call_indirect $tb (type $t) (local.get 0)))
  (func (export "tail") (param i64) (result i32)
    (return_call_indirect $tb (type $t) (local.get 0)))
  (func (export "get_is_null") (param i64) (result i32)
    (ref.is_null (table.get $tb (local.get 0))))
  (func (export "set_null") (param i64)
    (table.set $tb (local.get 0) (ref.null func)))
  (func (export "fill") (param i64)
    (table.fill $tb (local.get 0) (ref.null func) (i64.const 1)))
  (func (export "copy") (param i64)
    (table.copy $tb $tb (local.get 0) (i64.const 0) (i64.const 1)))
  (func (export "size") (result i64)
    (table.size $tb)))

;; The ordinary answers, so a trap below cannot pass by accident.
(assert_return (invoke "call" (i64.const 0)) (i32.const 100))
(assert_return (invoke "call" (i64.const 1)) (i32.const 101))
(assert_return (invoke "tail" (i64.const 1)) (i32.const 101))
(assert_return (invoke "get_is_null" (i64.const 0)) (i32.const 0))
(assert_return (invoke "size") (i64.const 2))

;; 2^32 is out of bounds on a two-slot table. Truncated to 32 bits it is slot 0 — which exists,
;; holds `$zero`, and answers 100. That is the wrong answer this file exists to pin.
(assert_trap (invoke "call" (i64.const 0x1_0000_0000)) "out of bounds table access")
(assert_trap (invoke "tail" (i64.const 0x1_0000_0000)) "out of bounds table access")
(assert_trap (invoke "get_is_null" (i64.const 0x1_0000_0000)) "out of bounds table access")
(assert_trap (invoke "set_null" (i64.const 0x1_0000_0000)) "out of bounds table access")
(assert_trap (invoke "fill" (i64.const 0x1_0000_0000)) "out of bounds table access")
(assert_trap (invoke "copy" (i64.const 0x1_0000_0000)) "out of bounds table access")
;; …and 2^32 + 1 is slot 1 once truncated, which holds a DIFFERENT function: a probe that only
;; tried 2^32 could not tell "trapped" from "called the first entry".
(assert_trap (invoke "call" (i64.const 0x1_0000_0001)) "out of bounds table access")

;; An i32 index on a 64-bit table is invalid for the tail form exactly as for the plain one.
(assert_invalid
  (module
    (type $t (func))
    (table $tb i64 1 funcref)
    (func (i32.const 0) (return_call_indirect $tb (type $t))))
  "type mismatch")
(assert_invalid
  (module
    (type $t (func))
    (table $tb i64 1 funcref)
    (func (i32.const 0) (call_indirect $tb (type $t))))
  "type mismatch")
;; …and an i64 index on a 32-bit table is invalid too, so neither type is simply accepted.
(assert_invalid
  (module
    (type $t (func))
    (table $tb 1 funcref)
    (func (i64.const 0) (return_call_indirect $tb (type $t))))
  "type mismatch")
