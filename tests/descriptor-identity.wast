;; custom-descriptors D2 — a `describes`/`descriptor` link is part of a type's IDENTITY.
;;
;; `$ad` below is a struct that DESCRIBES `$a`; `$q` is a plain struct of the same shape. They are
;; different types. If the canonical key dropped the links they would collapse into one type, and each
;; cast below would succeed on the other's value — type confusion at the level of type identity, which
;; no subtyping check downstream could catch (they would agree it is one type). The roadmap names
;; THREE identity keys: module-local canonicalisation, and the store-wide registry — so the same
;; property is asserted within one instance and across two.
;;
;; (`$ad` has no descriptor of its own, so plain `struct.new` may build it; `$a` could not be.)
;;
;; ⚠️ `$b`/`$bd` is the case that matters: a rec group of EXACTLY `{$a, $ad}`'s shape with the links
;; removed. `$q` alone cannot test the property — it is a singleton group, so its key differs from
;; `$ad`'s on group SHAPE whether or not the links are keyed, and the first version of this file (which
;; had only `$q`) passed with the links deleted from the key. Mutation found it.

(module $P
  (rec
    (type $a (descriptor $ad) (struct))
    (type $ad (describes $a) (struct)))
  (rec
    (type $b (struct))
    (type $bd (struct)))
  (type $q (struct))
  (func (export "mk-bd") (result anyref) (struct.new $bd))
  (func (export "ad-as-bd") (result i32) (ref.test (ref $bd) (struct.new $ad)))
  (func (export "bd-as-ad") (result i32) (ref.test (ref $ad) (struct.new $bd)))
  (func (export "mk-ad") (result anyref) (struct.new $ad))
  (func (export "mk-q") (result anyref) (struct.new $q))
  (func (export "ad-as-q") (result i32) (ref.test (ref $q) (struct.new $ad)))
  (func (export "q-as-ad") (result i32) (ref.test (ref $ad) (struct.new $q)))
  (func (export "ad-as-ad") (result i32) (ref.test (ref $ad) (struct.new $ad)))
  (func (export "q-as-q") (result i32) (ref.test (ref $q) (struct.new $q)))
)
(assert_return (invoke "ad-as-bd") (i32.const 0))
(assert_return (invoke "bd-as-ad") (i32.const 0))
(assert_return (invoke "ad-as-q") (i32.const 0))
(assert_return (invoke "q-as-ad") (i32.const 0))
(assert_return (invoke "ad-as-ad") (i32.const 1))
(assert_return (invoke "q-as-q") (i32.const 1))

;; Across instances: the importer spells the SAME types independently, so only the store-wide
;; registry can relate them — and it must keep `$ad` and `$q` apart just as the exporter does.
(register "P" $P)
(module
  (rec
    (type $a (descriptor $ad) (struct))
    (type $ad (describes $a) (struct)))
  (rec
    (type $b (struct))
    (type $bd (struct)))
  (type $q (struct))
  (import "P" "mk-ad" (func $mk_ad (result anyref)))
  (import "P" "mk-bd" (func $mk_bd (result anyref)))
  (import "P" "mk-q" (func $mk_q (result anyref)))
  (func (export "x-ad-as-bd") (result i32) (ref.test (ref $bd) (call $mk_ad)))
  (func (export "x-bd-as-ad") (result i32) (ref.test (ref $ad) (call $mk_bd)))
  (func (export "x-bd-as-bd") (result i32) (ref.test (ref $bd) (call $mk_bd)))
  (func (export "x-ad-as-ad") (result i32) (ref.test (ref $ad) (call $mk_ad)))
  (func (export "x-ad-as-q") (result i32) (ref.test (ref $q) (call $mk_ad)))
  (func (export "x-q-as-ad") (result i32) (ref.test (ref $ad) (call $mk_q)))
  (func (export "x-q-as-q") (result i32) (ref.test (ref $q) (call $mk_q)))
)
(assert_return (invoke "x-ad-as-ad") (i32.const 1))
(assert_return (invoke "x-ad-as-bd") (i32.const 0))
(assert_return (invoke "x-bd-as-ad") (i32.const 0))
(assert_return (invoke "x-bd-as-bd") (i32.const 1))
(assert_return (invoke "x-ad-as-q") (i32.const 0))
(assert_return (invoke "x-q-as-ad") (i32.const 0))
(assert_return (invoke "x-q-as-q") (i32.const 1))

;; A type WITH a descriptor cannot be built by plain `struct.new`, in a body or an initializer.
(assert_invalid
  (module
    (rec (type $a (descriptor $ad) (struct)) (type $ad (describes $a) (struct)))
    (func (drop (struct.new $a))))
  "type with descriptor requires descriptor allocation")
(assert_invalid
  (module
    (rec (type $a (descriptor $ad) (struct)) (type $ad (describes $a) (struct)))
    (global (ref null $a) (struct.new_default $a)))
  "type with descriptor requires descriptor allocation")
