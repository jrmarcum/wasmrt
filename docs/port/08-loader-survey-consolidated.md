# universalWasmLoader — consolidated binding survey (all 10)

## Three substrates in play (NOT one)
- wasmtime: c, dotnet, py, rs, v, zig
- wazero: go
- Chicory (pure-JVM interp): jvm
- host/browser WebAssembly: js, dart
Every loader hand-rolls Canonical ABI over a CORE module (cabi_realloc/cabi_post_<name>/callee-alloc [ptr,len]). NO loader uses any component-model API (rs's AbiKind::Component is a label; detect_abi just checks for a cabi_realloc export).

## Binding taxonomy vs wasmrt.h
| Loader | Runtime | Binds | Served by wasmrt? |
| c | wasmtime C API | direct C ABI | YES direct (wasmrt.h) |
| v | wasmtime C API via C loader | C ABI uwl_* | YES direct |
| zig | wasmtime C API via C loader | C ABI uwl_* | YES direct |
| dotnet | Wasmtime NuGet v44 → libwasmtime C ABI | native pkg on C ABI | YES *if* wasmrt ships libwasmtime-ABI-compatible shared lib |
| py | wasmtime-py >=26 (ctypes) → libwasmtime C ABI | native pkg on C ABI | YES *if* libwasmtime-ABI-compatible shared lib |
| rs | wasmtime CRATE v28 (Rust API) | native Rust API | NO C ABI — needs a wasmrt Rust crate (Engine/Linker/Store/Instance/Caller) OR rs refactors to FFI. **wasmrt IS Rust → native crate is free.** |
| go | wazero v1.12 | foreign runtime | NO (different runtime; out of scope for drop-in) |
| jvm | Chicory v1.0 | foreign runtime | NO |
| js | host WebAssembly | engine-provided | NO (off-web only relevant) |
| dart | browser WebAssembly | web-only | NO |

Leverage: c/v/zig/dotnet/py all ride the wasmtime C ABI → an ABI-compatible libwasmtime drops under 5/10 unmodified. rs is the 6th wasmtime consumer but via the Rust crate. go/jvm/js/dart never used wasmtime → out of scope for drop-in.

## TWO DISTINCT SCOPES (keep separate)
1. Host-API surface the loaders CALL → drives wasmrt.h. SMALL (below).
2. wasm MODULE features the runtime must EXECUTE → full wasmtime browser-standard parity (SIMD/multi-mem/threads/tail-calls/EH, WASI p1). A module may use SIMD internally even though the host API exposes no SIMD calls. Both hold — don't conflate.

## Union of host-API capabilities wasmrt must expose (to match wasmtime for its consumers)
1. Engine + compile module from raw bytes.
2. Store/context with per-instance linear memory.
3. Linker: named host-func define (define_func / func_new / host-module "env"), define_wasi, define_unknown_imports_as_traps (or ignore-unused-imports), instantiate.
4. WASI p1 subset actually imported: fd_write (iovec→stdio), proc_exit, random_get, clock_time_get, environ_sizes_get/environ_get, args_sizes_get/args_get, fd_close, fd_fdstat_get, fd_seek. Config: inherit stdout/stderr only (no preopens; env/args report empty). (jvm/go use full inheritSystem — strict superset.)
5. Reactor init: call _initialize once post-instantiate.
6. Export invoke by name: typed args i32/i64/f32/f64 (bool as i32), 0 or 1 result. Typed fast path for cabi_realloc:(i32,i32,i32,i32)->i32, cabi_post_<name>:(i32)->(), __malloc:(i32)->i32.
7. Linear-memory read/write raw data+data_size, INCL. from inside a host callback via a Caller/caller-export "memory" handle.
8. Exported global read (version i32; wasic __str_ret_ptr/__str_ret_len).
9. Host callback: decoded args in, 1 result out, guest-memory access at call time.
10. Traps & errors with retrievable messages.

## NOT needed by any loader's host API (can omit from wasmrt.h, still parity)
memory.grow (explicit), tables/table.get/indirect, MULTI-VALUE returns (all cap at 1 result), fuel/epoch, threads/shared memory, SIMD host calls, entire component-model/resource API. rs & jvm "wasic" path needs only global-read + a __malloc call (already in union).

## GAPS to flag for the Zig-origin wasmrt
- **Host func with caller-accessible guest memory + a return value** = the single most load-bearing capability beyond instantiate+call. wazmrt's CURRENT C ABI is the STANDARD wasm-c-api, whose wasm_func_callback_t gives the callback NO caller/memory handle — a real gap. wasmrt.h must adopt wasmtime's **caller-based** host-callback model (wasmtime_caller_context / caller_export_get("memory")). The interp layer already supports it (HostFunc.native_env with ctx), but the C-ABI surface must expose it. Breaks dotnet/go/jvm/py/rs/js/dart if missing. c/v/zig won't catch it (v/zig pass NULL callbacks).
- rs binds the Rust crate not C ABI → C-ABI-only wasmrt leaves rs unserved unless refactored; but wasmrt-core Rust crate serves it naturally.
- WASI fd_write iovec semantics (gather [ptr,len] pairs, write nwritten) must be exact — every I/O module depends on it. wazmrt has it.
- Rest (globals, _initialize, cabi_realloc/cabi_post, trap messages) = ordinary core-wasm surface wazmrt already has.

## DECISION this forces (for the owner)
ABI strategy for wasmrt.h:
(A) Exact wasmtime-C-ABI-compatible (same wasm_*/wasmtime_*/wasi_config_* symbol names + shapes) → drops under c/v/zig AND (with a lib swap) dotnet/py unmodified; but constrains wasmrt.h to wasmtime's exact shape/names (bigger, less "ours"); dotnet/py still need the package to load our lib.
(B) Clean lean wasmrt_* C ABI (wasmtime-SHAPED but our names) → c/v/zig port with a rename (owner owns them); + a native wasmrt Rust crate serves rs for free (wasmrt is Rust); dotnet/py get thin FFI/bindings later.
Recommend (B) + native Rust crate — matches "public face is wasmrt.h, not function-for-function the same," and rs-via-crate is a free win. Confirm with owner.
