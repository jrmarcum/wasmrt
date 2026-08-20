;; A global is a store ADDRESS. Importing one binds the address, so a `global.set` through the
;; import is visible to the exporter and to every other importer of the same global.
;;
;; ⚠️ wasmrt bound imported globals **by value**, and said so in a comment — a copy, so a write
;; through one import was invisible everywhere else. It is the same argument the owner made for
;; memories at T9a#4 ("the memory needs to be shared once pulled in"), one resource over, and the
;; suite states it as `instance.wast`'s "Import is not generative" section.

(module $M
  (global (export "g") (mut i32) (i32.const 0))
  (func (export "read") (result i32) (global.get 0))
)
(register "M" $M)

(module
  ;; The SAME global, imported twice. If either import were a copy these would be two cells.
  (import "M" "g" (global $a (mut i32)))
  (import "M" "g" (global $b (mut i32)))
  (func (export "write-a-read-b") (param i32) (result i32)
    (global.set $a (local.get 0))
    (global.get $b))
  (func (export "write-b") (param i32)
    (global.set $b (local.get 0)))
)

(assert_return (invoke "write-a-read-b" (i32.const 7)) (i32.const 7))
;; …and the EXPORTER sees it too, which a copy could never manage.
(assert_return (invoke $M "read") (i32.const 7))
(invoke "write-b" (i32.const 9))
(assert_return (invoke $M "read") (i32.const 9))
