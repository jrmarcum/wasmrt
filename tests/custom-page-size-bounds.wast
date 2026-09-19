;; custom-page-sizes — BYTE-GRANULAR bounds, on every access path.
;;
;; Track P's required check (roadmap, "Track P"): a memory of 1-byte pages must refuse an
;; out-of-bounds access at BYTE granularity. The spec suite probes one path (`i32.load8_u`); a
;; bounds check anywhere that still assumed 64 KiB pages would let a guest address up to 65,536x
;; the memory it has, and the suite could not see it. So every access class is walked to the edge
;; of a 3-byte memory here: loads and stores of every width, the offset immediate, SIMD, atomics,
;; the bulk ops, grow-then-access, active data segments, and memory64.
;;
;; Cross-checked against wasmtime 48 (`wasmtime wast -W custom-page-sizes=y`), which agrees on
;; every assertion — the expected answers are not wasmrt's alone.

(module
  (memory 3 (pagesize 1))
  (data $d "abcd")
  (func (export "size") (result i32) memory.size)
  (func (export "grow") (param i32) (result i32) (memory.grow (local.get 0)))
  (func (export "load8") (param i32) (result i32) (i32.load8_u (local.get 0)))
  (func (export "load8_off2") (param i32) (result i32) (i32.load8_u offset=2 (local.get 0)))
  (func (export "load8_off3") (param i32) (result i32) (i32.load8_u offset=3 (local.get 0)))
  (func (export "load16") (param i32) (result i32) (i32.load16_u (local.get 0)))
  (func (export "load32") (param i32) (result i32) (i32.load (local.get 0)))
  (func (export "load64") (param i32) (result i64) (i64.load (local.get 0)))
  (func (export "loadf32") (param i32) (result f32) (f32.load (local.get 0)))
  (func (export "loadf64") (param i32) (result f64) (f64.load (local.get 0)))
  (func (export "loadv128") (param i32) (drop (v128.load (local.get 0))))
  (func (export "store8") (param i32) (i32.store8 (local.get 0) (i32.const 7)))
  (func (export "store16") (param i32) (i32.store16 (local.get 0) (i32.const 7)))
  (func (export "storev128") (param i32) (v128.store (local.get 0) (v128.const i64x2 0 0)))
  (func (export "fill") (param i32 i32) (memory.fill (local.get 0) (i32.const 9) (local.get 1)))
  (func (export "copy") (param i32 i32 i32) (memory.copy (local.get 0) (local.get 1) (local.get 2)))
  (func (export "init") (param i32 i32 i32) (memory.init $d (local.get 0) (local.get 1) (local.get 2)))
  (func (export "atomic8") (param i32) (result i32) (i32.atomic.load8_u (local.get 0)))
  (func (export "atomic32") (param i32) (result i32) (i32.atomic.load (local.get 0)))
  (func (export "rmw8") (param i32) (result i32) (i32.atomic.rmw8.add_u (local.get 0) (i32.const 1)))
)

;; Three pages of ONE byte: three bytes, not 3 x 64 KiB.
(assert_return (invoke "size") (i32.const 3))

;; Single bytes: the last one is in, the next one is out.
(assert_return (invoke "load8" (i32.const 2)) (i32.const 0))
(assert_trap (invoke "load8" (i32.const 3)) "out of bounds memory access")
(assert_return (invoke "store8" (i32.const 2)))
(assert_trap (invoke "store8" (i32.const 3)) "out of bounds memory access")

;; The offset immediate counts toward the end, not only the operand.
(assert_return (invoke "load8_off2" (i32.const 0)) (i32.const 7))
(assert_trap (invoke "load8_off3" (i32.const 0)) "out of bounds memory access")

;; Wider accesses: a partial overlap with the end is out of bounds.
(assert_return (invoke "load16" (i32.const 1)) (i32.const 0x0700))
(assert_trap (invoke "load16" (i32.const 2)) "out of bounds memory access")
(assert_trap (invoke "store16" (i32.const 2)) "out of bounds memory access")
(assert_trap (invoke "load32" (i32.const 0)) "out of bounds memory access")
(assert_trap (invoke "load64" (i32.const 0)) "out of bounds memory access")
(assert_trap (invoke "loadf32" (i32.const 0)) "out of bounds memory access")
(assert_trap (invoke "loadf64" (i32.const 0)) "out of bounds memory access")
(assert_trap (invoke "loadv128" (i32.const 0)) "out of bounds memory access")
(assert_trap (invoke "storev128" (i32.const 0)) "out of bounds memory access")

;; Bulk memory: zero-length at the end is in bounds; one byte past is not.
(assert_return (invoke "fill" (i32.const 2) (i32.const 1)))
(assert_return (invoke "fill" (i32.const 3) (i32.const 0)))
(assert_trap (invoke "fill" (i32.const 3) (i32.const 1)) "out of bounds memory access")
(assert_trap (invoke "fill" (i32.const 0) (i32.const 4)) "out of bounds memory access")
(assert_return (invoke "copy" (i32.const 1) (i32.const 0) (i32.const 2)))
(assert_trap (invoke "copy" (i32.const 1) (i32.const 0) (i32.const 3)) "out of bounds memory access")
(assert_return (invoke "init" (i32.const 0) (i32.const 0) (i32.const 3)))
(assert_return (invoke "load8" (i32.const 2)) (i32.const 0x63))
(assert_trap (invoke "init" (i32.const 0) (i32.const 0) (i32.const 4)) "out of bounds memory access")

;; Atomics.
(assert_return (invoke "atomic8" (i32.const 2)) (i32.const 0x63))
(assert_trap (invoke "atomic32" (i32.const 0)) "out of bounds memory access")
(assert_trap (invoke "rmw8" (i32.const 3)) "out of bounds memory access")

;; Growing by ONE page adds ONE byte — and only that byte becomes addressable.
(assert_return (invoke "grow" (i32.const 1)) (i32.const 3))
(assert_return (invoke "size") (i32.const 4))
(assert_return (invoke "load32" (i32.const 0)) (i32.const 0x00636261))
(assert_return (invoke "load8" (i32.const 3)) (i32.const 0))
(assert_trap (invoke "load8" (i32.const 4)) "out of bounds memory access")

;; A maximum is in pages of the memory's own size.
(module
  (memory 3 5 (pagesize 1))
  (func (export "grow") (param i32) (result i32) (memory.grow (local.get 0))))
(assert_return (invoke "grow" (i32.const 3)) (i32.const -1))
(assert_return (invoke "grow" (i32.const 2)) (i32.const 3))
(assert_return (invoke "grow" (i32.const 1)) (i32.const -1))

;; An active data segment one byte past the end traps at instantiation.
(assert_trap
  (module (memory 3 (pagesize 1)) (data (i32.const 1) "abc"))
  "out of bounds memory access")
(module (memory 3 (pagesize 1)) (data (i32.const 0) "abc"))

;; memory64 with 1-byte pages: the same edge, 64-bit index.
(module
  (memory i64 3 (pagesize 1))
  (func (export "size") (result i64) memory.size)
  (func (export "load8") (param i64) (result i32) (i32.load8_u (local.get 0)))
  (func (export "load32") (param i64) (result i32) (i32.load (local.get 0)))
  (func (export "grow") (param i64) (result i64) (memory.grow (local.get 0))))
(assert_return (invoke "size") (i64.const 3))
(assert_return (invoke "load8" (i64.const 2)) (i32.const 0))
(assert_trap (invoke "load8" (i64.const 3)) "out of bounds memory access")
(assert_trap (invoke "load32" (i64.const 0)) "out of bounds memory access")
;; A delta whose byte count does not fit the address space is refused, never wrapped.
(assert_return (invoke "grow" (i64.const -1)) (i64.const -1))
(assert_return (invoke "size") (i64.const 3))
