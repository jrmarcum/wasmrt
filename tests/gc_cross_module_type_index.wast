;; A's ONLY type is index 0: a struct of i32.
(module $A
  (type $a0 (struct (field i32)))
  (global (export "g") anyref (struct.new $a0 (i32.const 111)))
)
(register "A" $A)

;; B's index 0 is a DIFFERENT type (i64 field); B's index 1 is the one that
;; structurally matches A's object.
(module $B
  (type $b0 (struct (field i64)))
  (type $b1 (struct (field i32)))
  (global $fromA (import "A" "g") anyref)
  ;; A's object carries type_index 0 (A's numbering). If that index is read
  ;; against B's type table, `is_subtype(0,0)` is trivially true and this says 1
  ;; — but the object is (struct i32) and $b0 is (struct i64).
  (func (export "wrongType") (result i32) (ref.test (ref $b0) (global.get $fromA)))
  ;; …and the structurally CORRECT answer, B's $b1, should be 1.
  (func (export "rightType") (result i32) (ref.test (ref $b1) (global.get $fromA)))
)
(assert_return (invoke $B "wrongType") (i32.const 0))   ;; not a (struct i64)
(assert_return (invoke $B "rightType") (i32.const 1))   ;; it IS a (struct i32)
