;; §4.4.5: a declared local's default is `t.default`, and a REFERENCE type's default is
;; `ref.null` — not zero. Locals were zero-filled, so an uninitialized `(ref null $t)` local read
;; as **GC heap object 0**.
;;
;; ⚠️⚠️ The fixture allocates first, on purpose. With an empty heap the same defect traps
;; `GcOutOfBounds` and reads like a slightly mislabelled pass; it only shows as SILENT WRONG OUTPUT
;; once the store holds an object at index 0 — which, in a `.wast` script, any earlier module
;; supplies. That is why `struct.wast` and `array.wast` caught it and no unit test did.

(module
  (type $s (struct (field i32) (field (mut i32))))
  (type $a (array (mut i32)))

  ;; Runs FIRST, so the object it makes is GC heap index 0 — the value a zero-filled local holds.
  (func (export "seed") (result i32)
    (struct.get $s 0 (struct.new $s (i32.const 111) (i32.const 222))))

  (func (export "struct.get-null") (result i32)
    (local (ref null $s)) (struct.get $s 1 (local.get 0)))
  (func (export "struct.set-null")
    (local (ref null $s)) (struct.set $s 1 (local.get 0) (i32.const 0)))
  (func (export "array.get-null") (result i32)
    (local (ref null $a)) (array.get $a (local.get 0) (i32.const 0)))
  (func (export "array.set-null")
    (local (ref null $a)) (array.set $a (local.get 0) (i32.const 0) (i32.const 0)))
  (func (export "len-null") (result i32)
    (local (ref null $a)) (array.len (local.get 0)))

  ;; The local is null on entry, so `ref.is_null` must say so.
  (func (export "is-null") (result i32)
    (local (ref null $s)) (ref.is_null (local.get 0)))
  ;; …and a funcref local too — same rule, different hierarchy, and this one would read as
  ;; function index 0 of instance 0 rather than as a heap object.
  (func (export "funcref-is-null") (result i32)
    (local funcref) (ref.is_null (local.get 0)))
  (func (export "externref-is-null") (result i32)
    (local externref) (ref.is_null (local.get 0)))
)

(assert_return (invoke "seed") (i32.const 111))

(assert_return (invoke "is-null") (i32.const 1))
(assert_return (invoke "funcref-is-null") (i32.const 1))
(assert_return (invoke "externref-is-null") (i32.const 1))

(assert_trap (invoke "struct.get-null") "null structure reference")
(assert_trap (invoke "struct.set-null") "null structure reference")
(assert_trap (invoke "array.get-null") "null array reference")
(assert_trap (invoke "array.set-null") "null array reference")
(assert_trap (invoke "len-null") "null array reference")
