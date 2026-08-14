/*
 * wasmrt.h — the public C ABI for the wasmrt WebAssembly runtime.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Jon Marcum
 *
 * A small, wasmtime-SHAPED engine surface under our own `wasmrt_*` names. It is NOT the
 * standard wasm-c-api and NOT wasmtime's exact symbols: wasmrt deliberately avoids the
 * wasm-c-api refcounted object model (wazmrt's single highest-risk file) in favour of
 * lightweight handles — see "Handles and ownership" below.
 *
 * SCOPE. This is the ENGINE surface. Callers that speak the Component Model do the
 * Canonical ABI marshalling themselves, on top of these primitives — writing arguments
 * into guest memory through the module's exported `cabi_realloc`, reading callee-allocated
 * [ptr,len] returns, calling `cabi_post_<name>`. That marshalling is the embedder's job,
 * not the runtime's, which is why nothing here mentions WIT or components.
 *
 * The engine EXECUTES far more than this API exposes. A guest may use SIMD, WasmGC,
 * threads/atomics, memory64 or exception handling internally and run correctly; the host
 * surface simply never hands those types across the boundary.
 *
 * ---------------------------------------------------------------------------------------
 * Handles and ownership
 * ---------------------------------------------------------------------------------------
 * Two kinds, and the distinction is the whole ownership story:
 *
 *   1. OPAQUE POINTERS (`wasmrt_engine_t *`, `wasmrt_module_t *`, …) — you own these, and
 *      each has exactly one `_delete`. Deleting twice, or using one after deleting it, is
 *      undefined behaviour, as it would be for any C API.
 *
 *   2. VALUE HANDLES (`wasmrt_instance_t`, `wasmrt_func_t`, `wasmrt_memory_t`,
 *      `wasmrt_global_t`) — small copyable structs naming something a store owns. You do
 *      NOT free them and there is no `_delete`. Copy them freely.
 *
 *      Value handles are CHECKED, not trusted. Each carries the identity of the store it
 *      came from, so a handle used with the wrong store, or one left over from a deleted
 *      store, is rejected — you get `false` or an error, never another store's resource.
 *      That is the property the refcount model exists to provide, obtained instead by
 *      construction. `wasmrt_*_is_valid` asks explicitly.
 *
 * THREADING. A store and everything reachable from it is single-threaded. Do not touch one
 * store from two threads, even under an external lock; separate stores are independent.
 *
 * ---------------------------------------------------------------------------------------
 * Error convention
 * ---------------------------------------------------------------------------------------
 *   - Functions returning `wasmrt_error_t *` return NULL on SUCCESS. A non-NULL result is
 *     an error you own and must `wasmrt_error_delete`.
 *   - A guest TRAP is not an error: it is reported through a `wasmrt_trap_t **` out-param,
 *     which is set only when the call actually trapped. A call can therefore return NULL
 *     (no host-side error) and still have trapped — always check both.
 *   - Functions returning `bool` return false for "not found" / "out of range", with no
 *     allocation.
 */
#ifndef WASMRT_H
#define WASMRT_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- Version / ABI handshake ---------------------------------------------------------
 * Check `wasmrt_abi_version()` against WASMRT_ABI_VERSION at load time when binding
 * dynamically; a mismatch means the header and the library disagree about this file.
 */
#define WASMRT_ABI_VERSION 1u

uint32_t    wasmrt_abi_version(void);     /* bumped on any breaking change to this header */
const char *wasmrt_version_string(void);  /* e.g. "0.9.0"; static storage, do not free     */

/* ---- Opaque handles (owned; each has exactly one _delete) ---------------------------- */
typedef struct wasmrt_engine      wasmrt_engine_t;
typedef struct wasmrt_config      wasmrt_config_t;
typedef struct wasmrt_store       wasmrt_store_t;
typedef struct wasmrt_module      wasmrt_module_t;
typedef struct wasmrt_linker      wasmrt_linker_t;
typedef struct wasmrt_functype    wasmrt_functype_t;
typedef struct wasmrt_trap        wasmrt_trap_t;   /* a guest trap                         */
typedef struct wasmrt_error       wasmrt_error_t;  /* a host-side error (compile/link/misuse) */
typedef struct wasmrt_caller      wasmrt_caller_t; /* VALID ONLY during a host callback    */
typedef struct wasmrt_wasi_config wasmrt_wasi_config_t;

/* ---- Value handles (not owned; no _delete; checked on use) ---------------------------- */
typedef struct { uint64_t id; } wasmrt_instance_t;
typedef struct { uint64_t id; } wasmrt_func_t;
typedef struct { uint64_t id; } wasmrt_memory_t;
typedef struct { uint64_t id; } wasmrt_global_t;

/* Does this handle still name something in `store`? False for a handle from another
 * store, from a deleted store, or never initialized. */
bool wasmrt_instance_is_valid(const wasmrt_store_t *, wasmrt_instance_t);
bool wasmrt_func_is_valid(const wasmrt_store_t *, wasmrt_func_t);
bool wasmrt_memory_is_valid(const wasmrt_store_t *, wasmrt_memory_t);
bool wasmrt_global_is_valid(const wasmrt_store_t *, wasmrt_global_t);

/* ---- Values ---------------------------------------------------------------------------
 * The types that cross the host boundary. A guest may use `v128` and the GC reference
 * types internally; they simply cannot be passed to or from a host call. Attempting to
 * call an export whose signature contains one returns an error rather than marshalling
 * something wrong.
 */
typedef enum {
    WASMRT_I32      = 0,
    WASMRT_I64      = 1,
    WASMRT_F32      = 2,
    WASMRT_F64      = 3,
    WASMRT_FUNCREF  = 4,  /* opaque to the host: pass it back unchanged */
    WASMRT_EXTERNREF = 5  /* likewise */
} wasmrt_valkind_t;

typedef struct {
    wasmrt_valkind_t kind;
    union {
        int32_t  i32;
        int64_t  i64;
        float    f32;
        double   f64;
        uint64_t ref;
    } of;
} wasmrt_val_t;

/* The kind of an import or export. */
typedef enum {
    WASMRT_EXTERN_FUNC   = 0,
    WASMRT_EXTERN_TABLE  = 1,
    WASMRT_EXTERN_MEMORY = 2,
    WASMRT_EXTERN_GLOBAL = 3,
    WASMRT_EXTERN_TAG    = 4
} wasmrt_externkind_t;

/* ---- Config: proposals + resource ceilings --------------------------------------------
 *
 * A config is a template; `wasmrt_engine_new_with_config` copies it. You still own the
 * config afterwards and must delete it.
 */

/* The WebAssembly proposals that can be individually refused. ALL ARE ON BY DEFAULT —
 * wasmrt's stated scope is full browser-standard parity plus memory64.
 *
 * There is deliberately NO tail-call entry: `return_call` / `return_call_indirect` are not
 * implemented, so a toggle for them would gate nothing while reading as a control.
 * `return_call_ref` belongs to function-references and is covered by that flag.
 *
 * A disabled proposal makes a module INVALID — it is refused by `wasmrt_module_new` /
 * `wasmrt_module_validate`, never part-way through execution.
 */
typedef enum {
    WASMRT_FEATURE_SIGN_EXTENSION            = 0,
    WASMRT_FEATURE_SATURATING_FLOAT_TO_INT   = 1,
    WASMRT_FEATURE_MULTI_VALUE               = 2,
    WASMRT_FEATURE_REFERENCE_TYPES           = 3,
    WASMRT_FEATURE_BULK_MEMORY               = 4,
    WASMRT_FEATURE_EXTENDED_CONST            = 5,
    WASMRT_FEATURE_SIMD                      = 6,
    WASMRT_FEATURE_RELAXED_SIMD              = 7,   /* requires SIMD                    */
    WASMRT_FEATURE_THREADS                   = 8,
    WASMRT_FEATURE_MULTI_MEMORY              = 9,
    WASMRT_FEATURE_MEMORY64                  = 10,
    WASMRT_FEATURE_FUNCTION_REFERENCES       = 11,  /* requires REFERENCE_TYPES         */
    WASMRT_FEATURE_GC                        = 12,  /* requires FUNCTION_REFERENCES     */
    WASMRT_FEATURE_EXCEPTIONS                = 13,  /* requires REFERENCE_TYPES         */
    WASMRT_FEATURE_TAIL_CALL                 = 14   /* return_call/_indirect            */
} wasmrt_feature_t;

wasmrt_config_t *wasmrt_config_new(void);
void wasmrt_config_delete(wasmrt_config_t *);

/* One setter rather than fourteen: adding a proposal later must not add a symbol, and the
 * enum keeps the exported surface small (a stated goal — see cmem/vision.md).
 * Returns false for an unrecognised `wasmrt_feature_t`. */
bool wasmrt_config_set_feature(wasmrt_config_t *, wasmrt_feature_t, bool enabled);
bool wasmrt_config_get_feature(const wasmrt_config_t *, wasmrt_feature_t, bool *out);

/* Turn every proposal on (the default) or off (WebAssembly 1.0 only). */
void wasmrt_config_all_features(wasmrt_config_t *, bool enabled);

/* Resource ceilings. Each is a CEILING, not a reservation: raising one costs nothing until
 * a guest actually asks for the memory. Passing 0 leaves the current value unchanged. */
void wasmrt_config_set_max_memory_bytes(wasmrt_config_t *, uint64_t);
void wasmrt_config_set_max_table_elements(wasmrt_config_t *, uint64_t);
void wasmrt_config_set_max_gc_objects(wasmrt_config_t *, uint64_t);
void wasmrt_config_set_max_exception_boxes(wasmrt_config_t *, uint64_t);

/* Guest call depth before the engine reports "call stack exhausted". Default 512.
 *
 * WORTH SETTING IF YOU LINK A DEBUG BUILD. The interpreter recurses on the host stack, and
 * an un-inlined debug frame is large enough that the default 512 can exhaust an 8 MiB
 * thread stack before the limit fires. Release builds are fine at the default, which is
 * why the default is not simply lowered. */
void wasmrt_config_set_max_call_depth(wasmrt_config_t *, uint32_t);

/* ---- Engine ---------------------------------------------------------------------------
 * Holds the configuration shared by the stores made from it. Must outlive them.
 */
wasmrt_engine_t *wasmrt_engine_new(void);                 /* defaults: every proposal on */

/* Returns NULL and sets *error if the config is incoherent — a proposal enabled without
 * one it is layered on (GC without function-references, relaxed SIMD without SIMD). Such a
 * config is REPORTED rather than quietly repaired: silently enabling a dependency would
 * accept modules you meant to refuse. You still own `cfg`. */
wasmrt_engine_t *wasmrt_engine_new_with_config(const wasmrt_config_t *cfg,
                                               wasmrt_error_t **error);
void wasmrt_engine_delete(wasmrt_engine_t *);

/* ---- Store ----------------------------------------------------------------------------
 * Owns instances and every memory, table and global they use. Instances in ONE store can
 * import from each other and genuinely share those resources; instances in different
 * stores are isolated. Must not outlive its engine.
 */
wasmrt_store_t *wasmrt_store_new(wasmrt_engine_t *);
void wasmrt_store_delete(wasmrt_store_t *);

/* ---- Module: compile, validate, introspect ------------------------------------------- */

/* Decode and validate `bytes`. Returns NULL on success, writing the module to *out; on
 * failure returns an error and leaves *out untouched. The bytes are not retained. */
wasmrt_error_t *wasmrt_module_new(wasmrt_engine_t *, const uint8_t *bytes, size_t len,
                                  wasmrt_module_t **out);

/* Would `wasmrt_module_new` succeed? No allocation, no module produced. */
bool wasmrt_module_validate(wasmrt_engine_t *, const uint8_t *bytes, size_t len);

void wasmrt_module_delete(wasmrt_module_t *);

/* Exports. `name` is borrowed and valid for the module's lifetime; it is NOT
 * NUL-terminated-safe to assume — use `*name_len_out`. Returns false if `i` is out of
 * range. */
size_t wasmrt_module_export_count(const wasmrt_module_t *);
bool   wasmrt_module_export(const wasmrt_module_t *, size_t i,
                            const char **name_out, size_t *name_len_out,
                            wasmrt_externkind_t *kind_out);

/* Imports, in DECLARATION ORDER — the order a linker must satisfy them in. */
size_t wasmrt_module_import_count(const wasmrt_module_t *);
bool   wasmrt_module_import(const wasmrt_module_t *, size_t i,
                            const char **module_out, size_t *module_len_out,
                            const char **name_out, size_t *name_len_out,
                            wasmrt_externkind_t *kind_out);

/* ---- Function types ------------------------------------------------------------------- */
wasmrt_functype_t *wasmrt_functype_new(const wasmrt_valkind_t *params, size_t nparams,
                                       const wasmrt_valkind_t *results, size_t nresults);
void   wasmrt_functype_delete(wasmrt_functype_t *);
size_t wasmrt_functype_param_count(const wasmrt_functype_t *);
size_t wasmrt_functype_result_count(const wasmrt_functype_t *);
bool   wasmrt_functype_param(const wasmrt_functype_t *, size_t i, wasmrt_valkind_t *out);
bool   wasmrt_functype_result(const wasmrt_functype_t *, size_t i, wasmrt_valkind_t *out);

/* ---- Host callbacks -------------------------------------------------------------------
 *
 * CALLER-BASED, which is the load-bearing difference from the standard wasm-c-api: that
 * API's callback gets no handle to the caller's memory, and essentially every real host
 * import needs to read guest memory and return a value.
 *
 * Return NULL for success, or a `wasmrt_trap_t *` to trap the guest (ownership transfers
 * to the engine — do not delete it). Write exactly `nresults` values into `results`.
 *
 * `caller` is valid ONLY for the duration of the call. Storing it and using it later is
 * undefined behaviour.
 */
typedef wasmrt_trap_t *(*wasmrt_func_callback_t)(
    void *env, wasmrt_caller_t *caller,
    const wasmrt_val_t *args, size_t nargs,
    wasmrt_val_t *results, size_t nresults);

/* The calling instance's exported memory, by name (conventionally "memory"). Returns false
 * if there is no such export. */
bool wasmrt_caller_get_memory(wasmrt_caller_t *, const char *name, wasmrt_memory_t *out);

/* Bounds-checked access to the caller's memory without needing a handle first — the common
 * case inside a callback. False if the range is out of bounds; nothing is copied. */
bool wasmrt_caller_read(wasmrt_caller_t *, uint64_t offset, void *dst, size_t n);
bool wasmrt_caller_write(wasmrt_caller_t *, uint64_t offset, const void *src, size_t n);
size_t wasmrt_caller_memory_size(wasmrt_caller_t *);

/* ---- Linker ---------------------------------------------------------------------------
 *
 * Resolves a module's imports BY NAME, walking them in declaration order. Reusable: define
 * a host surface once and instantiate many modules against it. Must not outlive its engine.
 */
wasmrt_linker_t *wasmrt_linker_new(wasmrt_engine_t *);
void wasmrt_linker_delete(wasmrt_linker_t *);

/* Define `module`.`name` as a host function. The linker copies the name strings and takes
 * ownership of nothing else; `env` is passed to every call, and `env_finalizer` (may be
 * NULL) runs when the linker is deleted or the name redefined. Redefining replaces. */
wasmrt_error_t *wasmrt_linker_define_func(wasmrt_linker_t *,
                                          const char *module, const char *name,
                                          const wasmrt_functype_t *,
                                          wasmrt_func_callback_t, void *env,
                                          void (*env_finalizer)(void *env));

wasmrt_error_t *wasmrt_linker_define_global(wasmrt_linker_t *,
                                            const char *module, const char *name,
                                            wasmrt_val_t value);

/* Publish every export of an already-instantiated module under the namespace `module`, so
 * later modules can import from it. The callee runs against ITS OWN instance, so it sees
 * the exporter's memory and globals. `instance` must belong to the store you later
 * instantiate into. */
wasmrt_error_t *wasmrt_linker_define_instance(wasmrt_linker_t *, const char *module,
                                              wasmrt_instance_t instance);

/* Provide WASI preview 1, using `cfg` (which the linker copies; you still own it). */
wasmrt_error_t *wasmrt_linker_define_wasi(wasmrt_linker_t *, const wasmrt_wasi_config_t *cfg);

/* Back every otherwise-unsatisfied FUNCTION import with a stub that traps if called.
 *
 * Useful because a module often declares more than it uses. The cost is real: with this
 * set, a typo'd import name is no longer a link-time error but a runtime surprise. Prefer
 * explicit definitions. */
wasmrt_error_t *wasmrt_linker_define_unknown_imports_as_traps(wasmrt_linker_t *);

/* Resolve and instantiate into `store`, running the module's start function if it has one.
 * A reactor's `_initialize` is NOT called — see `wasmrt_instance_initialize`.
 *
 * Returns an error for a link failure (naming the unresolved import). If the start
 * function traps, returns NULL and sets *trap_out. Check both. */
wasmrt_error_t *wasmrt_linker_instantiate(wasmrt_linker_t *, wasmrt_store_t *,
                                          const wasmrt_module_t *,
                                          wasmrt_instance_t *out,
                                          wasmrt_trap_t **trap_out);

/* ---- WASI preview 1 -------------------------------------------------------------------
 *
 * A guest reaches NOTHING it was not explicitly granted. With no preopened directory every
 * path call fails with BADF — there is no implicit working directory.
 */
wasmrt_wasi_config_t *wasmrt_wasi_config_new(void);
void wasmrt_wasi_config_delete(wasmrt_wasi_config_t *);

void wasmrt_wasi_config_inherit_stdout(wasmrt_wasi_config_t *);
void wasmrt_wasi_config_inherit_stderr(wasmrt_wasi_config_t *);
void wasmrt_wasi_config_inherit_stdin(wasmrt_wasi_config_t *);

/* argv/envp as NUL-terminated strings. Copied. */
void wasmrt_wasi_config_set_args(wasmrt_wasi_config_t *, const char *const *argv, size_t n);
void wasmrt_wasi_config_set_env(wasmrt_wasi_config_t *,
                                const char *const *names, const char *const *values, size_t n);

/* Grant `host_path`, visible to the guest as `guest_path`. Returns an error if the
 * directory cannot be opened. */
wasmrt_error_t *wasmrt_wasi_config_preopen_dir(wasmrt_wasi_config_t *,
                                               const char *host_path, const char *guest_path,
                                               bool read_only);

/* The exit code a guest passed to `proc_exit`, if it called it. Read after instantiate or
 * a call returns. */
bool wasmrt_wasi_exit_code(const wasmrt_linker_t *, int32_t *out);

/* ---- Instance exports ----------------------------------------------------------------- */
bool wasmrt_instance_get_func(wasmrt_store_t *, wasmrt_instance_t, const char *name,
                              wasmrt_func_t *out);
bool wasmrt_instance_get_memory(wasmrt_store_t *, wasmrt_instance_t, const char *name,
                                wasmrt_memory_t *out);
bool wasmrt_instance_get_global(wasmrt_store_t *, wasmrt_instance_t, const char *name,
                                wasmrt_global_t *out);

/* The reactor convention: call `_initialize` once after instantiating, if it is exported.
 * A no-op returning NULL when it is not. */
wasmrt_error_t *wasmrt_instance_initialize(wasmrt_store_t *, wasmrt_instance_t,
                                           wasmrt_trap_t **trap_out);

/* ---- Calling exports ------------------------------------------------------------------ */

/* The function's signature. Owned by the caller; delete it. NULL if the handle is invalid. */
wasmrt_functype_t *wasmrt_func_type(const wasmrt_store_t *, wasmrt_func_t);

/* Call it. `args`/`results` are caller-provided buffers; `nresults` must match the
 * function's result count exactly.
 *
 * Returns an error for MISUSE (bad handle, wrong arity, a type the boundary cannot carry).
 * Returns NULL and sets *trap_out if the GUEST trapped. Returns NULL with *trap_out
 * untouched on success. */
wasmrt_error_t *wasmrt_func_call(wasmrt_store_t *, wasmrt_func_t,
                                 const wasmrt_val_t *args, size_t nargs,
                                 wasmrt_val_t *results, size_t nresults,
                                 wasmrt_trap_t **trap_out);

/* ---- Linear memory ---------------------------------------------------------------------
 *
 * Two ways in. The raw view is the fast path embedders marshalling by hand want; the
 * checked copies are there when you would rather not reason about the invalidation rule.
 */

/* Raw view of guest memory.
 *
 * INVALIDATION — read this. The returned pointer is invalidated by anything that can grow
 * or replace the memory: `wasmrt_func_call`, `wasmrt_instance_initialize`,
 * `wasmrt_linker_instantiate` on the same store, and a guest `memory.grow` inside any of
 * them. Re-fetch after each; never cache it across a call. Returns NULL for an invalid
 * handle. */
uint8_t *wasmrt_memory_data(wasmrt_store_t *, wasmrt_memory_t);
size_t   wasmrt_memory_data_size(const wasmrt_store_t *, wasmrt_memory_t);
uint64_t wasmrt_memory_size_pages(const wasmrt_store_t *, wasmrt_memory_t);

/* Bounds-checked copies. False if the range is out of bounds or the handle is invalid; on
 * false nothing is copied. No pointer escapes, so there is no invalidation rule to get
 * wrong. */
bool wasmrt_memory_read(const wasmrt_store_t *, wasmrt_memory_t,
                        uint64_t offset, void *dst, size_t n);
bool wasmrt_memory_write(wasmrt_store_t *, wasmrt_memory_t,
                         uint64_t offset, const void *src, size_t n);

/* ---- Globals ---------------------------------------------------------------------------
 * A snapshot of the value. False for an invalid handle or a type the boundary cannot carry.
 */
bool wasmrt_global_get(const wasmrt_store_t *, wasmrt_global_t, wasmrt_val_t *out);

/* ---- Traps ----------------------------------------------------------------------------- */

/* Create a trap to return from a host callback. `message` is copied and may be NULL.
 *
 * Returning one from a `wasmrt_func_callback_t` transfers ownership to the engine — do NOT
 * delete it yourself. Delete only a trap the engine handed to YOU through a `trap_out`
 * parameter. */
wasmrt_trap_t *wasmrt_trap_new(const char *message);

/* NUL-terminated, borrowed until the trap is deleted. */
const char *wasmrt_trap_message(const wasmrt_trap_t *);
void        wasmrt_trap_delete(wasmrt_trap_t *);

/* Stack frames, innermost first. Snapshotted when the trap was created, so they stay valid
 * for as long as the trap does — you may keep it across further calls.
 *
 * A trap raised by one of YOUR host callbacks reports zero frames: it did not come out of
 * guest code, so there is no guest stack to show. `wasmrt_trap_message` always has the reason.
 *
 * `func_index_out` indexes the instance's function space, imports included — the same
 * numbering `ref.func` and the name section use.
 *
 * `offset_out` is the byte offset of the trapping instruction FROM THE START OF THE MODULE,
 * which is what `wasm-objdump` prints, so it can be looked up directly with no rebasing.
 *
 * `name_out` receives the function's name from the name section, or NULL if the guest was
 * built stripped. Borrowed until the trap is deleted.
 *
 * Every out-parameter is optional — pass NULL for any you do not want. `wasmrt_trap_frame`
 * returns false, writing nothing, for an index at or past the count. */
size_t wasmrt_trap_frame_count(const wasmrt_trap_t *);
bool   wasmrt_trap_frame(const wasmrt_trap_t *, size_t i,
                         uint32_t *func_index_out, uint32_t *offset_out,
                         const char **name_out);

/* ---- Errors ----------------------------------------------------------------------------- */
const char *wasmrt_error_message(const wasmrt_error_t *);
void        wasmrt_error_delete(wasmrt_error_t *);

#ifdef __cplusplus
} /* extern "C" */
#endif
#endif /* WASMRT_H */
