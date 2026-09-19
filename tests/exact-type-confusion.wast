;; custom-descriptors D1 — the SOUNDNESS CHECKPOINT: exact types must never admit a subtype.
;;
;; The roadmap's requirement, verbatim: "every cast arm needs a targeted WRONG-ANSWER test, not an
;; assertion count." An exact cast that answers YES to a subtype is type confusion — the guest holds a
;; value whose type it proved it did not have — and the spec corpus exercises the POSITIVE cases far
;; more than the negative ones. wasmtime 48 does not implement custom-descriptors, so it cannot referee
;; this file; every assertion here is a NEGATIVE the design must hold, each paired with the positive
;; that proves the path is really reached.
;;
;; A real hole of exactly this shape existed during D1 before the assembler could spell `(exact …)`:
;; every arm of `ref_matches` routed a non-`Concrete` target into the ABSTRACT path, where an exact
;; target resolved to its family — so `ref.cast (ref (exact $t))` would have accepted ANY struct.

;; --- GC arm, same instance: all four cast instructions ---------------------------------------
(module
  (type $super (sub (struct (field i32))))
  (type $sub (sub $super (struct (field i32) (field i64))))
  (type $other (struct (field i32)))

  (func $mk_super (result (ref $super)) (struct.new $super (i32.const 1)))
  (func $mk_sub (result (ref $super)) (struct.new $sub (i32.const 2) (i64.const 3)))
  (func $mk_other (result anyref) (struct.new $other (i32.const 4)))

  ;; ref.test
  (func (export "test-super") (result i32) (ref.test (ref (exact $super)) (call $mk_super)))
  (func (export "test-sub") (result i32) (ref.test (ref (exact $super)) (call $mk_sub)))
  (func (export "test-other") (result i32) (ref.test (ref (exact $super)) (call $mk_other)))
  (func (export "test-sub-inexact") (result i32) (ref.test (ref $super) (call $mk_sub)))
  (func (export "test-null") (result i32) (ref.test (ref null (exact $super)) (ref.null none)))
  ;; ref.cast
  (func (export "cast-super") (drop (ref.cast (ref (exact $super)) (call $mk_super))))
  (func (export "cast-sub") (drop (ref.cast (ref (exact $super)) (call $mk_sub))))
  (func (export "cast-other") (drop (ref.cast (ref (exact $super)) (call $mk_other))))
  ;; br_on_cast: 1 = branched (matched), 0 = fell through
  (func (export "br-super") (result i32)
    (block $l (result (ref (exact $super)))
      (br_on_cast $l anyref (ref (exact $super)) (call $mk_super)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  (func (export "br-sub") (result i32)
    (block $l (result (ref (exact $super)))
      (br_on_cast $l anyref (ref (exact $super)) (call $mk_sub)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  ;; br_on_cast_fail: 1 = branched (did NOT match), 0 = fell through (matched)
  (func (export "brf-super") (result i32)
    (block $l (result anyref)
      (br_on_cast_fail $l anyref (ref (exact $super)) (call $mk_super)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
  (func (export "brf-sub") (result i32)
    (block $l (result anyref)
      (br_on_cast_fail $l anyref (ref (exact $super)) (call $mk_sub)) (drop) (return (i32.const 0)))
    (drop) (i32.const 1))
)
(assert_return (invoke "test-super") (i32.const 1))
(assert_return (invoke "test-sub") (i32.const 0))
(assert_return (invoke "test-other") (i32.const 0))
(assert_return (invoke "test-sub-inexact") (i32.const 1))
(assert_return (invoke "test-null") (i32.const 1))
(assert_return (invoke "cast-super"))
(assert_trap (invoke "cast-sub") "cast failure")
(assert_trap (invoke "cast-other") "cast failure")
(assert_return (invoke "br-super") (i32.const 1))
(assert_return (invoke "br-sub") (i32.const 0))
(assert_return (invoke "brf-super") (i32.const 0))
(assert_return (invoke "brf-sub") (i32.const 1))

;; --- func arm, same instance --------------------------------------------------------------------
(module
  (type $fsuper (sub (func)))
  (type $fsub (sub $fsuper (func)))
  (func $a (type $fsuper))
  (func $b (type $fsub))
  (elem declare func $a $b)
  (func (export "ftest-exact") (result i32) (ref.test (ref (exact $fsuper)) (ref.func $a)))
  (func (export "ftest-sub") (result i32) (ref.test (ref (exact $fsuper)) (ref.func $b)))
  (func (export "ftest-sub-inexact") (result i32) (ref.test (ref $fsuper) (ref.func $b)))
  (func (export "fcast-sub") (drop (ref.cast (ref (exact $fsuper)) (ref.func $b))))
)
(assert_return (invoke "ftest-exact") (i32.const 1))
(assert_return (invoke "ftest-sub") (i32.const 0))
(assert_return (invoke "ftest-sub-inexact") (i32.const 1))
(assert_trap (invoke "fcast-sub") "cast failure")

;; --- ACROSS instances: the store-wide registry path, GC and func arms -----------------------------
(module $P
  (type $super (sub (struct (field i32))))
  (type $sub (sub $super (struct (field i32) (field i64))))
  (type $fsuper (sub (func)))
  (type $fsub (sub $fsuper (func)))
  (func (export "mk-super") (result anyref) (struct.new $super (i32.const 1)))
  (func (export "mk-sub") (result anyref) (struct.new $sub (i32.const 2) (i64.const 3)))
  (func $fa (export "fa") (type $fsuper))
  (func $fb (export "fb") (type $fsub))
  (elem declare func $fa $fb)
  (func (export "fref-a") (result funcref) (ref.func $fa))
  (func (export "fref-b") (result funcref) (ref.func $fb))
)
(register "P" $P)
(module
  (type $super (sub (struct (field i32))))
  (type $sub (sub $super (struct (field i32) (field i64))))
  (type $fsuper (sub (func)))
  (type $fsub (sub $fsuper (func)))
  (import "P" "mk-super" (func $mk_super (result anyref)))
  (import "P" "mk-sub" (func $mk_sub (result anyref)))
  (import "P" "fref-a" (func $fref_a (result funcref)))
  (import "P" "fref-b" (func $fref_b (result funcref)))
  (func (export "x-test-super") (result i32) (ref.test (ref (exact $super)) (call $mk_super)))
  (func (export "x-test-sub") (result i32) (ref.test (ref (exact $super)) (call $mk_sub)))
  (func (export "x-cast-sub") (drop (ref.cast (ref (exact $super)) (call $mk_sub))))
  (func (export "x-ftest-a") (result i32) (ref.test (ref (exact $fsuper)) (call $fref_a)))
  (func (export "x-ftest-b") (result i32) (ref.test (ref (exact $fsuper)) (call $fref_b)))
)
(assert_return (invoke "x-test-super") (i32.const 1))
(assert_return (invoke "x-test-sub") (i32.const 0))
(assert_trap (invoke "x-cast-sub") "cast failure")
(assert_return (invoke "x-ftest-a") (i32.const 1))
(assert_return (invoke "x-ftest-b") (i32.const 0))

;; --- link time: an EXACT import refuses a subtype, however it is reached ------------------------
(assert_unlinkable
  (module
    (type $fsuper (sub (func)))
    (type $fsub (sub $fsuper (func)))
    (import "P" "fb" (func (exact (type $fsuper)))))
  "incompatible import type")
(module
  (type $fsuper (sub (func)))
  (import "P" "fa" (func (exact (type $fsuper)))))

;; --- type IDENTITY: two types differing only in a field's exactness are DIFFERENT types ---------
;; If canonicalisation dropped the exact bit, $e and $i would be one type and each cast below would
;; succeed on the other's value.
(module
  (type $t (struct))
  (type $e (struct (field (ref null (exact $t)))))
  (type $i (struct (field (ref null $t))))
  (func (export "id-e-as-i") (result i32) (ref.test (ref $i) (struct.new $e (ref.null none))))
  (func (export "id-i-as-e") (result i32) (ref.test (ref $e) (struct.new $i (ref.null none))))
  (func (export "id-e-as-e") (result i32) (ref.test (ref $e) (struct.new $e (ref.null none))))
)
(assert_return (invoke "id-e-as-i") (i32.const 0))
(assert_return (invoke "id-i-as-e") (i32.const 0))
(assert_return (invoke "id-e-as-e") (i32.const 1))

;; --- validation: nothing inexact satisfies an exact type -----------------------------------------
(assert_invalid
  (module
    (type $t (struct))
    (func (param (ref $t)) (result (ref (exact $t))) (local.get 0)))
  "type mismatch")
(assert_invalid
  (module
    (type $f (func))
    (import "" "" (func $i (type $f)))
    (elem declare func $i)
    (func (result (ref (exact $f))) (ref.func $i)))
  "type mismatch")
