;; §3.3.5: `local.get` of a non-defaultable local requires that local to be SET — and
;; `unreachable` does not set anything. wasmrt exempted unreachable code from the check, so this
;; module validated; wasm-tools 1.259 and wasmtime 48 both refuse it, at the same offset, with
;; "uninitialized local: 0".
;;
;; 🎓 `unreachable` makes the VALUE STACK polymorphic. The local-init context is a different
;; context, and it is not touched — treating "the stack is anything" as "everything is known"
;; is the confusion this pins.
(assert_invalid
  (module
    (func (local (ref func))
      unreachable
      local.get 0
      drop))
  "uninitialized local")

;; …and the same shape after a `br`, which also makes the rest of the block unreachable.
(assert_invalid
  (module
    (func (local (ref func))
      (block (br 0) (drop (local.get 0)))))
  "uninitialized local")

;; The control: a local that IS set first, read in unreachable code, must still validate — the
;; check is about the init state, not about being in dead code.
(module
  (elem declare func $f)
  (func $f)
  (func (export "ok") (result funcref)
    (local $r (ref func))
    (local.set $r (ref.func $f))
    unreachable
    (local.get $r)))

;; A DEFAULTABLE local needs no initialization at all, in dead code or out of it.
(module
  (func (export "d") (result i32)
    (local i32)
    unreachable
    (local.get 0)))
