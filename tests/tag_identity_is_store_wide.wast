;; An exception's TAG is an identity, not a number. `Exception.tag` held the module-local tag
;; index, so tag 0 of one module matched tag 0 of another and a `catch $mine` caught an exception
;; thrown by somebody ELSE's tag — the same class as the funcref and GC-object index defects, and
;; like those it answers with a plausible value instead of failing loudly.
;;
;; ⚠️ It takes TWO modules to see. Within one module the indices coincide with the identities, so
;; every single-module exception test passes either way; `try_table.wast`'s `imported-mismatch` and
;; `legacy/try_catch.wast` were the only two assertions in 63,000 that could tell.

(module $thrower
  (tag $only (param i32))     ;; the thrower's tag index 0
  (func (export "boom") (param i32)
    (throw $only (local.get 0)))
)
(register "T" $thrower)

(module
  (func $boom (import "T" "boom") (param i32))
  (tag $mine (param i32))     ;; ALSO index 0 — a different tag with the same number

  ;; 1 = `catch $mine` fired (wrong: it is not our tag)
  ;; 2 = it did not, and the enclosing `catch_all` handled it (right)
  (func (export "foreign") (result i32)
    (block $outer
      (try_table (catch_all $outer)
        (block $inner (result i32)
          (try_table (catch $mine $inner)
            (call $boom (i32.const 7)))
          (unreachable))
        (drop)
        (return (i32.const 1)))
      (return (i32.const 3)))
    (i32.const 2))

  ;; …and our own tag still matches, so the fix is not "never match".
  (func (export "own") (result i32)
    (block $h (result i32)
      (try_table (result i32) (catch $mine $h)
        (throw $mine (i32.const 5))
        (i32.const 100))))

  ;; A `catch_all` is tag-blind by definition and must still catch a foreign exception.
  (func (export "foreign-catch-all") (result i32)
    (block $h
      (try_table (catch_all $h)
        (call $boom (i32.const 9)))
      (return (i32.const 3)))
    (i32.const 4))
)

(assert_return (invoke "own") (i32.const 5))
(assert_return (invoke "foreign") (i32.const 2))
(assert_return (invoke "foreign-catch-all") (i32.const 4))
