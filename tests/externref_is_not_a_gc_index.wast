;; S1 — the externref bridge. The reason `any.convert_extern` / `extern.convert_any` were held
;; back is recorded here as an executable claim rather than a comment: **a host reference and a
;; GC heap index shared one numeric space.** An `externref` crosses the C ABI as a raw integer,
;; while a non-null, non-i31 `anyref` is read as an index into the GC heap — so the moment a host
;; handle could be internalized, host handle 2 read as GC object #2, and a `ref.cast` that
;; "verified" it let the following `struct.get` read a field at another type's width.
;;
;; ⚠️ The fixture pins the *dangerous* direction. Every assertion below would still pass if the
;; two instructions were implemented as no-ops on a bare integer — EXCEPT the ones that ask what
;; an internalized host reference IS, which is exactly where the confusion lived.

(module
  (type $s (struct (field i32)))
  (type $a (array i8))

  (table $t 8 anyref)

  (func (export "init") (param $x externref)
    ;; Slot 0 is a real GC object, allocated FIRST so it takes heap index 0 — the same
    ;; number the host handle in slot 1 carries. If the two spaces were one, they would be
    ;; the same value.
    (table.set $t (i32.const 0) (struct.new $s (i32.const 42)))
    (table.set $t (i32.const 1) (any.convert_extern (local.get $x)))
    (table.set $t (i32.const 2) (any.convert_extern (ref.null extern)))
  )

  ;; Is the slot a `$s`? Host handle 0 must answer NO even though GC object 0 answers yes.
  (func (export "is_struct") (param $i i32) (result i32)
    (ref.test (ref $s) (table.get $t (local.get $i)))
  )
  ;; A host reference is `any` …
  (func (export "is_any") (param $i i32) (result i32)
    (ref.test (ref any) (table.get $t (local.get $i)))
  )
  ;; … and nothing narrower: NOT `eq`, so it can never be compared with `ref.eq`.
  (func (export "is_eq") (param $i i32) (result i32)
    (ref.test (ref eq) (table.get $t (local.get $i)))
  )
  (func (export "is_i31") (param $i i32) (result i32)
    (ref.test (ref i31) (table.get $t (local.get $i)))
  )
  (func (export "is_null") (param $i i32) (result i32)
    (ref.is_null (table.get $t (local.get $i)))
  )

  ;; Reading the object through a cast. Slot 0 is a genuine `$s`; slot 1 is a host handle with
  ;; the same numeric payload, and `ref.cast` must TRAP rather than hand `struct.get` a field.
  (func (export "read") (param $i i32) (result i32)
    (struct.get $s 0 (ref.cast (ref $s) (table.get $t (local.get $i))))
  )

  ;; Round trips, both directions.
  (func (export "out") (param $i i32) (result externref)
    (extern.convert_any (table.get $t (local.get $i)))
  )
  (func (export "there_and_back") (param $x externref) (result externref)
    (extern.convert_any (any.convert_extern (local.get $x)))
  )
  (func (export "back_and_there") (param $x externref) (result anyref)
    (any.convert_extern (extern.convert_any (any.convert_extern (local.get $x))))
  )
)

(invoke "init" (ref.extern 0))

;; The type-confusion assertions. Slot 0 is GC object 0; slot 1 is host handle 0.
(assert_return (invoke "is_struct" (i32.const 0)) (i32.const 1))
(assert_return (invoke "is_struct" (i32.const 1)) (i32.const 0))
(assert_return (invoke "is_any" (i32.const 0)) (i32.const 1))
(assert_return (invoke "is_any" (i32.const 1)) (i32.const 1))
(assert_return (invoke "is_eq" (i32.const 0)) (i32.const 1))
(assert_return (invoke "is_eq" (i32.const 1)) (i32.const 0))
(assert_return (invoke "is_i31" (i32.const 1)) (i32.const 0))

;; null externalizes and internalizes to null (§4.4.7.3) — it is NOT a wrapped value.
(assert_return (invoke "is_null" (i32.const 2)) (i32.const 1))
(assert_return (invoke "out" (i32.const 2)) (ref.null extern))

(assert_return (invoke "read" (i32.const 0)) (i32.const 42))
(assert_trap (invoke "read" (i32.const 1)) "cast failure")

;; A host reference internalizes to `(ref.host n)` and externalizes back to `(ref.extern n)`.
;; ⚠️ Asserting BOTH spellings is what makes the pair non-trivial: if `any.convert_extern` and
;; `extern.convert_any` were both the identity, one of these two must be wrong.
(assert_return (invoke "there_and_back" (ref.extern 7)) (ref.extern 7))
(assert_return (invoke "back_and_there" (ref.extern 7)) (ref.host 7))
(assert_return (invoke "out" (i32.const 1)) (ref.extern 0))
(assert_return (invoke "out" (i32.const 0)) (ref.extern))

;; An externref wrapping a GC object is still an `externref` and nothing else.
(assert_return (invoke "there_and_back" (ref.null extern)) (ref.null extern))
