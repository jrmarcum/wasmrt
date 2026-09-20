;; An imported TABLE links, and `call_indirect` through it dispatches into the reference's OWNER.
;;
;; This is the property T9a#4's decision gate was waiting on, and the one the whole `funcref`
;; encoding exists for: a `funcref` carries its owning instance in bits 62..32 (instance 0 packing
;; to the bare index, which is why landing the encoding moved the spec suite by +1/−1). Before that,
;; a `funcref` was a bare function index and `call_indirect` resolved it against the CALLING
;; instance — so the moment two instances shared a table, A's `ref.func` was called as B's function
;; of the same index. A silent wrong call, the class this project ranks worst.
;;
;; ⚠️ B holds a decoy at its own index 0 that answers a DIFFERENT number. Without it the test cannot
;; tell "dispatched to the owner" from "dispatched to whatever happened to be at that index", which
;; is the same distinction `funcref_cross_module_type_index.wast` makes for ref.test typing — that
;; file covers the TYPE half of the rule across a link; this one covers DISPATCH.

(module $TA
  (type $ft (func (result i32)))
  (table (export "t") 2 funcref)
  (func $f (type $ft) (i32.const 42))
  (elem (i32.const 0) $f)
  ;; A's own view of slot 1, so "the table is one object" can be asserted from both sides.
  (func (export "call1") (result i32)
    (call_indirect (type $ft) (i32.const 1))))
(register "TA" $TA)

(module $TB
  (type $ft (func (result i32)))
  (import "TA" "t" (table 2 funcref))
  ;; B's own function 0 — the wrong answer a caller-resolved dispatch would give.
  (func $decoy (type $ft) (i32.const 7))
  ;; C.refs: a `ref.func` in a body needs the function declared outside it.
  (elem declare func $decoy)
  (func (export "call0") (result i32)
    (call_indirect (type $ft) (i32.const 0)))
  (func (export "plant")
    (table.set 0 (i32.const 1) (ref.func $decoy)))
  (func (export "call1") (result i32)
    (call_indirect (type $ft) (i32.const 1))))

;; Slot 0 holds A's function: 42, not B's 7.
(assert_return (invoke $TB "call0") (i32.const 42))
;; Slot 1 is empty until B plants its own function there.
(assert_trap (invoke $TB "call1") "uninitialized element")
(invoke $TB "plant")
;; B planted its OWN function through the shared table, so this one IS B's 7 — the same mechanism
;; pointing the other way, which is what makes the 42 above meaningful.
(assert_return (invoke $TB "call1") (i32.const 7))
;; …and A sees the entry B planted, because the table is one object, not a copy.
(assert_return (invoke $TA "call1") (i32.const 7))
