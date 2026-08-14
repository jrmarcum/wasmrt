;; The funcref mirror of `gc_cross_module_type_index.wast`.
;;
;; The GC report noted that `ref_matches` had already learned "a reference's type lives in its owning
;; module" for funcrefs — but the Func arm only got HALF the rule. It fetched the type index from the
;; owner's module and then compared it against the TESTING module's table anyway, which its own comment
;; described as "approximate". That is the same defect the GC arm carried, and the full spec testsuite
;; never moved when it was fixed: nothing in 62,498 assertions casts a funcref to a concrete type
;; across a module boundary. So this file is the only thing standing between that fix and a silent
;; regression.
;;
;; A's ONLY type is index 0: (func (param i32)).
(module $FA
  (type $a0 (func (param i32)))
  (func $f (type $a0))
  (global (export "g") funcref (ref.func $f))
)
(register "FA" $FA)

;; B's index 0 is a DIFFERENT func type; B's index 1 is the one that actually matches.
(module $FB
  (type $b0 (func (param i64)))
  (type $b1 (func (param i32)))
  (global $fromA (import "FA" "g") funcref)
  ;; A's funcref carries type index 0 in A's numbering. Read against B's table, `is_subtype(0,0)` is
  ;; trivially true and this says 1 — but the function takes an i32 and $b0 takes an i64.
  (func (export "wrongType") (result i32) (ref.test (ref $b0) (global.get $fromA)))
  ;; …and B's $b1 is the structurally correct answer.
  (func (export "rightType") (result i32) (ref.test (ref $b1) (global.get $fromA)))
  ;; An ABSTRACT target must stay true regardless — it asks only "is this a func?", which involves no
  ;; type index at all. Pinned so that tightening the concrete path does not break the abstract one.
  (func (export "isFunc") (result i32) (ref.test funcref (global.get $fromA)))
)
(assert_return (invoke $FB "wrongType") (i32.const 0))  ;; not a (func (param i64))
(assert_return (invoke $FB "rightType") (i32.const 1))  ;; it IS a (func (param i32))
(assert_return (invoke $FB "isFunc") (i32.const 1))     ;; and it is certainly a func
