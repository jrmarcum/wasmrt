//! 🔒 **The execution bound** (T9i; `cmem/interop.md` §3.7a — owner, 2026-08-19: *"We do not want
//! an infinite loop on purpose or by accident … an error message to the user with a break on
//! occurrence."*).
//!
//! 🎯 **The UNIT is what has to match, not just the number**: one iteration is **one loop back-edge
//! or one tail-call hop**. Two runtimes with the same ceiling counting different things are not
//! swappable — a module finishing just under it on one would trap on the other — so these tests
//! assert the unit, not merely that something eventually stops.

use wasmrt_core::interp::{i32_value, Instance, ResourceLimits, Trap};

fn module(src: &str) -> wasmrt_core::module::Module {
    let bytes = wasmrt_core::wat::assemble(src.as_bytes()).expect("assemble");
    wasmrt_core::module::decode(&bytes).expect("decode")
}

fn with_budget(src: &str, max_iterations: u64) -> Instance {
    let limits = ResourceLimits {
        max_iterations,
        ..ResourceLimits::defaults()
    };
    Instance::new_with(module(src), wasmrt_core::interp::Imports::new(), limits).expect("instantiate")
}

const SPIN: &str = r#"(module
    (func (export "spin") (loop $l (br $l)))
    (func $f (export "tailspin") (return_call $f))
    (func (export "count") (param i32) (result i32)
      (local $i i32)
      (block $done (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get 0)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
      (local.get $i)))"#;

/// A loop that never exits is stopped, and the trap names the ceiling it hit.
#[test]
fn an_endless_loop_traps_on_the_budget() {
    let mut inst = with_budget(SPIN, 1_000);
    let err = inst.invoke("spin", &[]).expect_err("must not run forever");
    assert_eq!(err, Trap::IterationLimitExceeded { limit: 1_000 });
    let text = err.to_string();
    assert!(text.contains("1000"), "the ceiling must be stated: {text}");
    assert!(text.contains("--max-iterations"), "and how to raise it: {text}");
    // ⚠️ It must not claim to have DETECTED an infinite loop — a legitimately long run trips the
    // same trap, and its owner needs to be told to raise the ceiling, not told a falsehood.
    assert!(!text.to_lowercase().contains("infinite loop detected"), "{text}");
}

/// ⚠️⚠️ **The tail-call hop is the tick whose absence an obvious test cannot see.** A local
/// `return_call` reuses the frame: no back-edge, no call-depth growth. Removing this tick leaves
/// the loop test above passing while `(func $f (return_call $f))` runs forever.
#[test]
fn an_endless_tail_call_chain_traps_on_the_same_budget() {
    let mut inst = with_budget(SPIN, 1_000);
    assert_eq!(
        inst.invoke("tailspin", &[]),
        Err(Trap::IterationLimitExceeded { limit: 1_000 })
    );
}

/// The budget counts BACK-EDGES, so a loop of n iterations costs n — the unit both runtimes
/// must agree on. A run just under the ceiling finishes; one just over it traps.
#[test]
fn the_unit_is_one_back_edge_per_iteration() {
    // 100 iterations => 100 back-edges, plus the one that exits the loop.
    let mut ok = with_budget(SPIN, 200);
    assert_eq!(ok.invoke("count", &[i32_value(100)]), Ok(std::vec![i32_value(100)]));

    let mut tight = with_budget(SPIN, 50);
    assert_eq!(
        tight.invoke("count", &[i32_value(100)]),
        Err(Trap::IterationLimitExceeded { limit: 50 })
    );
}

/// The budget is per TOP-LEVEL invocation, refilled on entry — so a host loop calling many short
/// guest functions is never starved by the calls that came before it.
#[test]
fn the_budget_is_refilled_for_each_top_level_call() {
    let mut inst = with_budget(SPIN, 200);
    for _ in 0..10 {
        assert_eq!(
            inst.invoke("count", &[i32_value(100)]),
            Ok(std::vec![i32_value(100)]),
            "each call gets its own budget"
        );
    }
}

/// `0` disables the bound — and the loop that traps at a finite ceiling then completes.
#[test]
fn zero_means_unlimited() {
    let mut inst = with_budget(SPIN, 0);
    assert_eq!(
        inst.invoke("count", &[i32_value(1_000_000)]),
        Ok(std::vec![i32_value(1_000_000)]),
        "a million back-edges must run with the bound off"
    );
}

/// The default is far above the corpus's heaviest legitimate workload: measured, the peak is under
/// 2^20 back-edges (`return_call.wast`'s million-hop chain), and the default is 2^30.
#[test]
fn the_default_budget_does_not_trip_a_realistic_workload() {
    let mut inst = Instance::new(module(SPIN)).expect("instantiate");
    assert_eq!(
        inst.invoke("count", &[i32_value(2_000_000)]),
        Ok(std::vec![i32_value(2_000_000)])
    );
}
