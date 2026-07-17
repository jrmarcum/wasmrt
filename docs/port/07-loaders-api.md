# universalWasmLoader — API surface driving wasmrt.h

Loaders dir: `D:/Programs/_ProgramExamples/Example_Programs/GithubProjects/universalWasmLoader`
Ten language loaders: c, dart, dotnet, go, js, jvm, py, rs, v, zig. Currently built on **wasmtime v45 C API** (vendored).

## Architecture (KEY reconciliation)
The loaders are HIGH-LEVEL libraries, not low-level runtime consumers. Each:
- Loads a `.wasm` **reactor/library module produced by wasmtk** (TS→wasm via wasic/modc) + auto-detects a companion **`.wit` sidecar**.
- Applies the **Component-Model Canonical ABI ITSELF** over a CORE module (NOT via wasmtime's component API — C loader uses `wasmtime_component_*` ZERO times): writes UTF-8 string params into linear memory via the module's exported **`cabi_realloc`**, reads callee-allocated `[ptr,len]` returns, calls **`cabi_post_<name>`** to free. Conforms to cross-language **SPEC v3.0.0**.
- Exposes a simple native API (C: `uwl_import`/`uwl_call_i32`/`uwl_call_str`/`uwl_free`/host imports/singleton/pool/`uwl_last_error`).

**This is why the kickoff prompt talked about cabi_realloc/cabi_post_*/WIT/MAX_FLAT_RESULTS** — that is the LOADER layer's job, done on top of the runtime. wazmrt (the runtime) has none of it, correctly. wasmrt (the runtime port) REPLACES wasmtime as the engine the loaders sit on; the canonical-ABI marshalling stays in the loaders.

So: **wasmrt.h = the engine primitives the loaders call** (what they currently use from wasmtime's C API), NOT the canonical ABI itself.

## Exact C-ABI surface the C loader calls (universal_wasm_loader.h, ~38 fns) = the wasmrt.h requirement
Uses wasmtime's `wasmtime_*` store/context/linker/typed-val model (NOT the standard wasm-c-api instance/func model), plus a few standard `wasm_*` type constructors, plus `wasi_*`:
- Engine: wasm_engine_new / wasm_engine_delete
- Store+context: wasmtime_store_new / wasmtime_store_delete / wasmtime_store_context ; wasmtime_context_set_wasi
- Module: wasmtime_module_new (from bytes) / wasmtime_module_delete
- Linker (imports + instantiate): wasmtime_linker_new / delete / define_func (host imports) / define_wasi / define_unknown_imports_as_traps / instantiate
- Instance/exports: wasmtime_instance_export_get
- Call: wasmtime_func_call / wasmtime_func_type
- Globals: wasmtime_global_get
- **Linear memory (for canonical-ABI marshalling — load-bearing): wasmtime_memory_data / wasmtime_memory_data_size** (raw ptr + size into guest memory; loader writes/reads bytes + calls cabi_realloc/cabi_post)
- Values/types: wasmtime_val_t + wasmtime_val_unroot ; wasm_valtype_new / wasm_valtype_vec_new / _new_empty ; wasm_functype_new / delete / results
- Traps/errors: wasm_trap_message / delete ; wasmtime_error_message / delete ; wasmtime_extern_delete
- Host-callback caller access: wasmtime_caller_context / wasmtime_caller_export_get
- WASI config: wasi_config_new / wasi_config_inherit_stdout / wasi_config_inherit_stderr

Public uwl_ API (what the loader offers on top): uwl_import, uwl_call(+_i32/_i64/_f32/_f64/_bool/_void/_str), uwl_str/_strn, uwl_val_free/string_free, uwl_free, uwl_last_error, uwl_singleton_*, uwl_pool_* (module instance pooling).

## Implications for wasmrt.h design
- Mirror the **wasmtime `wasmtime_*` shape** (engine/store/context/module/linker/instance/func/memory/val/trap/error + wasi_config) for the ~38-fn subset above, so the C/Zig/V/Julia loaders port with prefix/rename churn only. The owner owns the loaders and will update them.
- MUST expose **raw linear-memory pointer + size** (the loaders marshal canonical ABI by hand) and the ability to **call exported funcs by name with typed vals** incl. `cabi_realloc`/`cabi_post_<name>`.
- MUST expose a **linker with host-func imports + WASI + unknown-imports-as-traps + instantiate**.
- WASI p1 (already in wazmrt). No component API needed.
- Module instance **pooling/singleton** is a loader concern (uwl_pool), but wasmrt must make instances cheap to create/reset (aligns with the arena/reset model + cold-start thesis).

## OPEN / to confirm (per-language)
The C loader binds the C ABI directly. rs/go/py/js/dotnet/jvm/dart likely use **native per-language wasmtime bindings** (wasmtime crate, wasmtime-go, wasmtime-py, etc.), not the C ABI. Replacing wasmtime for THOSE means either (a) they all FFI the C `wasmrt.h`, or (b) wasmrt ships native bindings per language. Zig/V/Julia bind the C ABI. NEEDS a cross-loader survey to confirm each loader's binding mechanism + whether any needs runtime capabilities beyond the C loader's 38-fn set.
