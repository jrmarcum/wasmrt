;; ⚠️ **The test the whole tail-call proposal exists for.**
;;
;; Conformance cannot check this. Every assertion in `return_call.wast`, `return_call_indirect.wast`
;; and `return_call_ref.wast` checks a RESULT, and a "call the callee, then jump to the end of the
;; body" implementation produces exactly the right results while growing the native stack on every
;; hop — which is the one thing tail calls are for. wasmrt shipped precisely that for
;; `return_call_ref` and scored 40/7 on its conformance file without anything noticing.
;;
;; So this file asserts the PROPERTY instead: a chain far longer than any native stack must return
;; normally. Each function is paired with an ordinary-`call` twin doing the identical arithmetic, so
;; the file also proves it can fail — those twins are what the depth would do without the feature.
;; (The twins are not invoked at this depth here; `interp.rs`'s unit tests cover the trap.)

(module
  ;; --- return_call: self-recursive, 1,000,000 deep -----------------------------------------
  (func $countdown (export "countdown") (param i64) (result i64)
    local.get 0
    i64.eqz
    if (result i64)
      i64.const 42
    else
      local.get 0
      i64.const 1
      i64.sub
      return_call $countdown
    end
  )

  ;; --- return_call: MUTUAL recursion, which is the case a self-call optimizer would miss ----
  (func $even (export "even") (param i64) (result i32)
    local.get 0
    i64.eqz
    if (result i32)
      i32.const 1
    else
      local.get 0
      i64.const 1
      i64.sub
      return_call $odd
    end
  )
  (func $odd (param i64) (result i32)
    local.get 0
    i64.eqz
    if (result i32)
      i32.const 0
    else
      local.get 0
      i64.const 1
      i64.sub
      return_call $even
    end
  )

  ;; --- return_call_indirect: the chain runs THROUGH a table --------------------------------
  (type $counter (func (param i64) (result i64)))
  (table $t 1 1 funcref)
  (elem (i32.const 0) $indirect_countdown)
  (func $indirect_countdown (type $counter)
    local.get 0
    i64.eqz
    if (result i64)
      i64.const 7
    else
      local.get 0
      i64.const 1
      i64.sub
      i32.const 0
      return_call_indirect $t (type $counter)
    end
  )
  (func (export "indirect") (param i64) (result i64)
    local.get 0
    return_call $indirect_countdown
  )

  ;; --- return_call_ref: through a typed function reference ---------------------------------
  (elem declare func $ref_countdown)
  (func $ref_countdown (type $counter)
    local.get 0
    i64.eqz
    if (result i64)
      i64.const 9
    else
      local.get 0
      i64.const 1
      i64.sub
      ref.func $ref_countdown
      return_call_ref $counter
    end
  )
  (func (export "viaref") (param i64) (result i64)
    local.get 0
    return_call $ref_countdown
  )

  ;; A tail call is still allowed to be shallow — the ordinary case must keep working.
  (func (export "shallow") (result i64) (return_call $countdown (i64.const 3)))
)

;; 1,000,000 frames. Native stack depth for an ordinary call is bounded by `max_call_depth` long
;; before this, so any of these completing at all is the proof.
(assert_return (invoke "countdown" (i64.const 1000000)) (i64.const 42))
(assert_return (invoke "even" (i64.const 1000000)) (i32.const 1))
(assert_return (invoke "even" (i64.const 999999)) (i32.const 0))
(assert_return (invoke "indirect" (i64.const 1000000)) (i64.const 7))
(assert_return (invoke "viaref" (i64.const 1000000)) (i64.const 9))
(assert_return (invoke "shallow") (i64.const 42))
