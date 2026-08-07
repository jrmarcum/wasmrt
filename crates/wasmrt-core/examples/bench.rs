//! wasmrt interpreter microbenchmark — the Rust counterpart of wazmrt's `bench/bench.zig`.
//!
//! Run: `cargo run --release -p wasmrt-core --example bench`
//!
//! Two regimes, reported **separately and never conflated** (`bench/README.md`):
//!
//! - **Cold path** — decode + validate + instantiate + one call. The per-run cost a
//!   short-lived program pays, and the regime wasmrt aims to win. Reported for a toy module
//!   *and* for a realistically sized one, because the toy figure is sub-millisecond and
//!   quoting it as a cold start would be dishonest.
//! - **Steady state** — a hot loop inside one instance: raw dispatch throughput. A JIT wins
//!   this regime; the number exists to inform the Option A → B decision, not to flatter.
//!
//! Compare only against a **same-session baseline** (A/B/A around a change). Run-to-run
//! spread on one machine is several percent, so cross-day comparisons are noise.

use std::time::{Duration, Instant};

use wasmrt_core::interp::{as_i32, i32_value, Instance};
use wasmrt_core::{module, validate, wat};

/// The canonical workload: `sum(n)` over a tight `loop`/`br_if`.
///
/// Thirteen instructions execute per iteration — counted from the body below, which is why
/// the body must not be edited without updating [`OPS_PER_ITER`].
const SUM_WAT: &str = r#"(module
  (func (export "sum") (param $n i32) (result i32)
    (local $i i32) (local $acc i32)
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $acc (i32.add (local.get $acc) (local.get $i)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $acc)))"#;

/// local.get·2 + i32.ge_s + br_if + local.get·2 + i32.add + local.set
/// + local.get + i32.const + i32.add + local.set + br
const OPS_PER_ITER: u64 = 13;

/// Build a module of `n` distinct functions, to get a cold-path figure at a realistic module
/// size. A toy module's decode time is dominated by fixed costs and says nothing useful.
fn big_module_wat(n: usize) -> String {
    let mut s = String::from("(module\n");
    for i in 0..n {
        s.push_str(&format!(
            "  (func (export \"f{i}\") (param i32 i32) (result i32)\n\
             \x20   (i32.add (i32.mul (local.get 0) (i32.const {i}))\n\
             \x20             (i32.sub (local.get 1) (i32.const {i}))))\n"
        ));
    }
    s.push_str(")\n");
    s
}

fn assemble(src: &str) -> Vec<u8> {
    wat::assemble(src.as_bytes()).expect("assemble")
}

/// Decode + validate + instantiate + one call, `iters` times. Returns the mean.
fn cold(bytes: &[u8], func: &str, args: &[u128], iters: u32) -> Duration {
    // One warm-up run outside the timer, so the first-touch page faults and any lazy
    // allocator growth are not charged to the mean.
    run_once(bytes, func, args);
    let t = Instant::now();
    for _ in 0..iters {
        run_once(bytes, func, args);
    }
    t.elapsed() / iters
}

fn run_once(bytes: &[u8], func: &str, args: &[u128]) {
    let md = module::decode(bytes).expect("decode");
    validate::validate(&md).expect("validate");
    let mut inst = Instance::new(md).expect("instantiate");
    inst.invoke(func, args).expect("invoke");
}

fn main() {
    println!("wasmrt bench — v{}\n", env!("CARGO_PKG_VERSION"));

    // ---- Cold path -------------------------------------------------------------------
    let toy = assemble(SUM_WAT);
    let d = cold(&toy, "sum", &[i32_value(1)], 2000);
    println!("cold   toy module   {:>8} bytes   {:>9.3} µs", toy.len(), d.as_secs_f64() * 1e6);

    // ~46 KB, chosen to match the size of the guest the oracle's recorded cold-start figure
    // used — a cold-start number is meaningless without the module size beside it.
    let big = assemble(&big_module_wat(1900));
    let d = cold(&big, "f0", &[i32_value(1), i32_value(2)], 200);
    println!(
        "cold   real module  {:>8} bytes   {:>9.3} ms   <- quote THIS as the cold start",
        big.len(),
        d.as_secs_f64() * 1e3
    );

    // ---- Steady state ----------------------------------------------------------------
    let md = module::decode(&toy).expect("decode");
    validate::validate(&md).expect("validate");
    let mut inst = Instance::new(md).expect("instantiate");

    // Warm up, and check the answer — a benchmark that computes the wrong thing measures
    // nothing. sum(0..n) = n(n-1)/2, taken mod 2^32 as i32.
    let warm = 1_000_000i32;
    let got = as_i32(inst.invoke("sum", &[i32_value(warm)]).expect("invoke")[0]);
    let want = (u64::from(warm as u32) * u64::from(warm as u32 - 1) / 2) as u32 as i32;
    assert_eq!(got, want, "workload computed the wrong sum");

    for n in [1_000_000i32, 10_000_000, 50_000_000] {
        let t = Instant::now();
        inst.invoke("sum", &[i32_value(n)]).expect("invoke");
        let e = t.elapsed();
        let ops = u64::from(n as u32) * OPS_PER_ITER;
        println!(
            "steady sum({n:>10})   {:>9.3} ms   {:>7.1} Mops/s",
            e.as_secs_f64() * 1e3,
            ops as f64 / e.as_secs_f64() / 1e6
        );
    }
}
