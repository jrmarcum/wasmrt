;; custom-descriptors D3/D4 — the SOUNDNESS CHECKPOINT for the `*_desc` instructions.
;;
;; A desc-eq cast succeeds by descriptor IDENTITY: the value's stored descriptor must BE the given
;; descriptor object. The spec corpus checks the successes thoroughly; the dangerous direction is a cast
;; that succeeds when it must not, so every assertion below is a NEGATIVE paired with the positive that
;; proves the path is reached. wasmtime 48 has no custom-descriptors, so it cannot referee this file;
;; its modules are checked by wasm-tools instead.

(module $P
  (rec
    (type $a (sub (descriptor $ad) (struct (field i32))))
    (type $ad (sub (describes $a) (struct))))
  (type $plain (struct (field i32)))
  ;; A FORGERY: an ordinary struct whose LAST field holds the descriptor reference. The descriptor slot
  ;; is also the last field, so only the owner type's declared descriptor tells the two apart.
  (type $fake (struct (field (ref $ad))))
  (global $d1 (export "d1") (ref (exact $ad)) (struct.new $ad))
  (global $d2 (export "d2") (ref (exact $ad)) (struct.new $ad))
  (func $obj1 (export "obj1") (result anyref) (struct.new_desc $a (i32.const 7) (global.get $d1)))

  ;; ref.cast_desc_eq
  (func (export "cast-same") (result i32)
    (struct.get $a 0 (ref.cast_desc_eq (ref $a) (call $obj1) (global.get $d1))))
  (func (export "cast-other-desc") (drop (ref.cast_desc_eq (ref $a) (call $obj1) (global.get $d2))))
  (func (export "cast-plain")
    (drop (ref.cast_desc_eq (ref $a) (struct.new $plain (i32.const 1)) (global.get $d1))))
  (func (export "cast-i31") (drop (ref.cast_desc_eq (ref $a) (ref.i31 (i32.const 1)) (global.get $d1))))
  ;; A HOST reference brought into `any` has no descriptor — refused before any heap read. (Not
  ;; `any.convert_extern (extern.convert_any x)`: that round trip IS `x` again, and legitimately passes.)
  (func (export "cast-host") (param externref)
    (drop (ref.cast_desc_eq (ref $a) (any.convert_extern (local.get 0)) (global.get $d1))))
  (func (export "cast-roundtrip") (result i32)
    (struct.get $a 0 (ref.cast_desc_eq (ref $a)
      (any.convert_extern (extern.convert_any (call $obj1))) (global.get $d1))))
  (func (export "cast-null-desc") (drop (ref.cast_desc_eq (ref null $a) (call $obj1) (ref.null none))))
  (func (export "cast-null-value") (result i32)
    (ref.is_null (ref.cast_desc_eq (ref null $a) (ref.null any) (global.get $d1))))
  (func (export "cast-forged") (drop (ref.cast_desc_eq (ref $a) (struct.new $fake (global.get $d1)) (global.get $d1))))
  (func (export "get-forged") (result i32)
    (ref.test (ref $a) (struct.new $fake (global.get $d1))))
  ;; The runner does not compare trap MESSAGES, so the null-descriptor check is pinned by shapes whose
  ;; outcome changes without it: a null value on a nullable target would PASS, a _fail branch would BRANCH.
  (func (export "cast-null-both")
    (drop (ref.cast_desc_eq (ref null $a) (ref.null any) (ref.null none))))
  (func (export "brf-null-desc") (result i32)
    (block $l (result anyref)
      (br_on_cast_desc_eq_fail $l anyref (ref $a) (call $obj1) (ref.null none)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  (func (export "cast-null-value-nn") (drop (ref.cast_desc_eq (ref $a) (ref.null any) (global.get $d1))))

  ;; br_on_cast_desc_eq: 1 = branched (matched)
  (func (export "br-same") (result i32)
    (block $l (result (ref $a))
      (br_on_cast_desc_eq $l anyref (ref $a) (call $obj1) (global.get $d1)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  (func (export "br-other") (result i32)
    (block $l (result (ref $a))
      (br_on_cast_desc_eq $l anyref (ref $a) (call $obj1) (global.get $d2)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  ;; br_on_cast_desc_eq_fail: 1 = branched (did NOT match)
  (func (export "brf-same") (result i32)
    (block $l (result anyref)
      (br_on_cast_desc_eq_fail $l anyref (ref $a) (call $obj1) (global.get $d1)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  (func (export "brf-other") (result i32)
    (block $l (result anyref)
      (br_on_cast_desc_eq_fail $l anyref (ref $a) (call $obj1) (global.get $d2)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))

  ;; ref.get_desc returns THAT descriptor, not merely one of the right type.
  (func (export "get-is-d1") (result i32)
    (ref.eq (ref.get_desc $a (ref.cast (ref $a) (call $obj1))) (global.get $d1)))
  (func (export "get-is-d2") (result i32)
    (ref.eq (ref.get_desc $a (ref.cast (ref $a) (call $obj1))) (global.get $d2)))
  (func (export "get-null") (drop (ref.get_desc $a (ref.null $a))))
  (func (export "new-null-desc") (drop (struct.new_desc $a (i32.const 0) (ref.null none))))
  ;; The descriptor slot is invisible to field access: field 0 is the declared i32.
  (func (export "field-0") (result i32)
    (struct.get $a 0 (struct.new_desc $a (i32.const 42) (global.get $d1))))
)
(assert_return (invoke "cast-same") (i32.const 7))
(assert_trap (invoke "cast-other-desc") "cast failure")
(assert_trap (invoke "cast-plain") "cast failure")
(assert_trap (invoke "cast-i31") "cast failure")
(assert_trap (invoke "cast-host" (ref.extern 1)) "cast failure")
(assert_return (invoke "cast-roundtrip") (i32.const 7))
(assert_trap (invoke "cast-null-desc") "null descriptor reference")
(assert_return (invoke "cast-null-value") (i32.const 1))
(assert_trap (invoke "cast-null-value-nn") "cast failure")
(assert_trap (invoke "cast-forged") "cast failure")
(assert_return (invoke "get-forged") (i32.const 0))
(assert_trap (invoke "cast-null-both") "null descriptor reference")
(assert_trap (invoke "brf-null-desc") "null descriptor reference")
(assert_return (invoke "br-same") (i32.const 1))
(assert_return (invoke "br-other") (i32.const 0))
(assert_return (invoke "brf-same") (i32.const 0))
(assert_return (invoke "brf-other") (i32.const 1))
(assert_return (invoke "get-is-d1") (i32.const 1))
(assert_return (invoke "get-is-d2") (i32.const 0))
(assert_trap (invoke "get-null") "null reference")
(assert_trap (invoke "new-null-desc") "null descriptor reference")
(assert_return (invoke "field-0") (i32.const 42))

;; ACROSS instances: the object and the descriptors come from $P; the casts are here.
(register "P" $P)
(module
  (rec
    (type $a (sub (descriptor $ad) (struct (field i32))))
    (type $ad (sub (describes $a) (struct))))
  (import "P" "d1" (global $d1 (ref (exact $ad))))
  (import "P" "d2" (global $d2 (ref (exact $ad))))
  (import "P" "obj1" (func $obj1 (result anyref)))
  (func (export "x-same") (result i32)
    (struct.get $a 0 (ref.cast_desc_eq (ref $a) (call $obj1) (global.get $d1))))
  (func (export "x-other") (drop (ref.cast_desc_eq (ref $a) (call $obj1) (global.get $d2))))
  (func (export "x-get-is-d1") (result i32)
    (ref.eq (ref.get_desc $a (ref.cast (ref $a) (call $obj1))) (global.get $d1)))
)
(assert_return (invoke "x-same") (i32.const 7))
(assert_trap (invoke "x-other") "cast failure")
(assert_return (invoke "x-get-is-d1") (i32.const 1))
