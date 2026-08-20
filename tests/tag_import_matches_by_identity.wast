;; An imported tag shares the exporter's tag IDENTITY, and §4.5.9 matches it on the DEFINED TYPE,
;; not on the signature.
;;
;; ⚠️ Two tags can both be `(func)` and still be different types: position within a rec group is
;; part of identity, and only the type INDEX carries it. That is the T9h finding — "comparing
;; SIGNATURES can never answer an IDENTITY question" — and the tag import check walked into it,
;; because the decoder resolved the import's typeidx to a `FuncType` and discarded the index.

(module $E
  (rec
    (type $t1 (func))
    (type $t2 (func)))
  (tag $e (export "tag") (type $t1))
  (func (export "boom") (throw $e))
)
(register "E" $E)

(module
  (rec
    (type $t1 (func))
    (type $t2 (func)))
  (func $boom (import "E" "boom"))
  ;; The SAME tag, by identity. A `catch` on it must catch what `$E` throws — which the
  ;; store-wide tag slot is what makes possible.
  (tag $mine (import "E" "tag") (type $t1))
  (func (export "catches") (result i32)
    (block $h
      (try_table (catch $mine $h)
        (call $boom))
      (return (i32.const 0)))
    (i32.const 1))
)

(assert_return (invoke "catches") (i32.const 1))

;; Same signature, different rec-group member: NOT the same type.
(assert_unlinkable
  (module
    (rec
      (type $t1 (func))
      (type $t2 (func)))
    (tag (import "E" "tag") (type $t2)))
  "incompatible import type"
)

;; Same shape, but a singleton group rather than a two-member one — a different type again.
(assert_unlinkable
  (module
    (type $t (func))
    (tag (import "E" "tag") (type $t)))
  "incompatible import type"
)
