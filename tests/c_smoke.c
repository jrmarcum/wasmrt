/*
 * c_smoke.c — the BEHAVIOURAL gate for `wasmrt.h`.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Jon Marcum
 *
 * Links the wasmrt static library and drives the C ABI the way a real embedder does:
 * configure -> compile -> link a host import -> instantiate -> call -> read and write
 * linear memory -> read a global -> take a trap -> read its message.
 *
 * WHY THIS EXISTS ALONGSIDE THE RUST TESTS. `crates/wasmrt-capi/src/tests.rs` calls the same
 * functions, but from Rust — which means the compiler still checked the types. This file is
 * compiled by a C compiler against the shipped header, so it proves three things Rust
 * cannot: that `wasmrt.h` is valid C, that its declarations MATCH the exported symbols, and
 * that the whole thing links. A signature that disagreed between header and library would
 * pass every Rust test and fail here.
 *
 * The module bytes below are hand-assembled rather than read from a file, so the gate has no
 * fixture dependency and fails for exactly one reason: the ABI.
 */
#include "wasmrt.h"

#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static int failures = 0;

#define CHECK(cond, ...)                                                       \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__);               \
            fprintf(stderr, __VA_ARGS__);                                      \
            fprintf(stderr, "\n");                                             \
            failures++;                                                        \
        }                                                                      \
    } while (0)

static void report_error(const char *what, wasmrt_error_t *e)
{
    if (e) {
        fprintf(stderr, "FAIL %s: %s\n", what, wasmrt_error_message(e));
        wasmrt_error_delete(e);
        failures++;
    }
}

/* ---------------------------------------------------------------------------------------
 * (module
 *   (import "env" "add_one" (func $ai (param i32) (result i32)))
 *   (memory (export "memory") 1)
 *   (global (export "answer") i32 (i32.const 42))
 *   (data (i32.const 0) "hi")
 *   (func (export "run") (param i32) (result i32) (call $ai (local.get 0)))
 *   (func (export "boom") (unreachable)))
 * ------------------------------------------------------------------------------------- */
static const unsigned char MODULE[] = {
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    /* 1 type: 0 = (i32)->i32, 1 = ()->() */
    0x01, 0x09, 0x02, 0x60, 0x01, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x00,
    /* 2 import: env.add_one : type 0 */
    0x02, 0x0f, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x07,
    0x61, 0x64, 0x64, 0x5f, 0x6f, 0x6e, 0x65, 0x00, 0x00,
    /* 3 function: two defined funcs, types 0 and 1 */
    0x03, 0x03, 0x02, 0x00, 0x01,
    /* 5 memory: 1 page */
    0x05, 0x03, 0x01, 0x00, 0x01,
    /* 6 global: i32 immutable = 42 */
    0x06, 0x06, 0x01, 0x7f, 0x00, 0x41, 0x2a, 0x0b,
    /* 7 export: memory, answer, run, boom */
    0x07, 0x20, 0x04,
    0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00,
    0x06, 0x61, 0x6e, 0x73, 0x77, 0x65, 0x72, 0x03, 0x00,
    0x03, 0x72, 0x75, 0x6e, 0x00, 0x01,
    0x04, 0x62, 0x6f, 0x6f, 0x6d, 0x00, 0x02,
    /* 12 data count */
    0x0c, 0x01, 0x01,
    /* 10 code */
    0x0a, 0x0c, 0x02,
    0x06, 0x00, 0x20, 0x00, 0x10, 0x00, 0x0b, /* run:  local.get 0; call 0; end */
    0x03, 0x00, 0x00, 0x0b,                   /* boom: unreachable; end         */
    /* 11 data: "hi" at 0 */
    0x0b, 0x08, 0x01, 0x00, 0x41, 0x00, 0x0b, 0x02, 0x68, 0x69,
};

/* The host import: adds the caller-supplied step to its argument, and proves both that
 * `env` arrives and that guest memory is reachable from inside a callback. */
static wasmrt_trap_t *add_one(void *env, wasmrt_caller_t *caller,
                              const wasmrt_val_t *args, size_t nargs,
                              wasmrt_val_t *results, size_t nresults)
{
    int32_t step = env ? *(int32_t *)env : 1;

    if (nargs != 1 || nresults != 1) {
        return wasmrt_trap_new("host callback got the wrong arity");
    }

    /* Read what the guest's data segment put at offset 0 — the caller-based callback is the
     * whole reason this ABI is not the standard wasm-c-api. */
    char buf[2] = {0, 0};
    if (!wasmrt_caller_read(caller, 0, buf, 2) || buf[0] != 'h' || buf[1] != 'i') {
        return wasmrt_trap_new("host callback could not read guest memory");
    }
    /* Write back through the same handle. */
    if (!wasmrt_caller_write(caller, 0, "HI", 2)) {
        return wasmrt_trap_new("host callback could not write guest memory");
    }

    results[0].kind = WASMRT_I32;
    results[0].of.i32 = args[0].of.i32 + step;
    return NULL;
}

static int env_finalized = 0;
static void note_finalizer(void *env) { (void)env; env_finalized = 1; }

int main(void)
{
    printf("wasmrt c_smoke: abi=%u version=%s\n",
           wasmrt_abi_version(), wasmrt_version_string());

    CHECK(wasmrt_abi_version() == WASMRT_ABI_VERSION,
          "ABI mismatch: library %u, header %u",
          wasmrt_abi_version(), WASMRT_ABI_VERSION);

    /* ---- config + engine ------------------------------------------------------------ */
    wasmrt_config_t *cfg = wasmrt_config_new();
    CHECK(cfg != NULL, "config_new returned NULL");
    bool on = false;
    CHECK(wasmrt_config_get_feature(cfg, WASMRT_FEATURE_SIMD, &on) && on,
          "SIMD should be on by default");
    CHECK(!wasmrt_config_set_feature(cfg, (wasmrt_feature_t)999, false),
          "an unknown feature index must be rejected");
    wasmrt_config_set_max_call_depth(cfg, 256);

    wasmrt_error_t *e = NULL;
    wasmrt_engine_t *engine = wasmrt_engine_new_with_config(cfg, &e);
    report_error("engine_new_with_config", e);
    CHECK(engine != NULL, "engine is NULL");
    wasmrt_config_delete(cfg);

    /* An incoherent config must be refused, not quietly repaired. */
    wasmrt_config_t *bad = wasmrt_config_new();
    wasmrt_config_set_feature(bad, WASMRT_FEATURE_FUNCTION_REFERENCES, false);
    e = NULL;
    wasmrt_engine_t *no_engine = wasmrt_engine_new_with_config(bad, &e);
    CHECK(no_engine == NULL, "GC without function-references must not produce an engine");
    CHECK(e != NULL, "an incoherent config must report why");
    if (e) { wasmrt_error_delete(e); }
    wasmrt_config_delete(bad);

    /* ---- module --------------------------------------------------------------------- */
    CHECK(wasmrt_module_validate(engine, MODULE, sizeof MODULE), "the test module must validate");

    wasmrt_module_t *module = NULL;
    e = wasmrt_module_new(engine, MODULE, sizeof MODULE, &module);
    report_error("module_new", e);
    CHECK(module != NULL, "module is NULL");
    if (!module) { return 1; }

    CHECK(wasmrt_module_export_count(module) == 4, "expected 4 exports, got %zu",
          wasmrt_module_export_count(module));
    CHECK(wasmrt_module_import_count(module) == 1, "expected 1 import");

    const char *imod = NULL, *inam = NULL;
    size_t imod_len = 0, inam_len = 0;
    wasmrt_externkind_t ikind;
    CHECK(wasmrt_module_import(module, 0, &imod, &imod_len, &inam, &inam_len, &ikind),
          "import 0 should be readable");
    CHECK(imod_len == 3 && memcmp(imod, "env", 3) == 0, "import module name wrong");
    CHECK(ikind == WASMRT_EXTERN_FUNC, "import kind wrong");

    /* ---- linker + host import -------------------------------------------------------- */
    wasmrt_linker_t *linker = wasmrt_linker_new(engine);
    CHECK(linker != NULL, "linker is NULL");

    wasmrt_valkind_t p[1] = {WASMRT_I32};
    wasmrt_valkind_t r[1] = {WASMRT_I32};
    wasmrt_functype_t *ft = wasmrt_functype_new(p, 1, r, 1);
    CHECK(wasmrt_functype_param_count(ft) == 1, "functype param count");
    CHECK(wasmrt_functype_result_count(ft) == 1, "functype result count");

    static int32_t step = 5;
    e = wasmrt_linker_define_func(linker, "env", "add_one", ft, add_one, &step, note_finalizer);
    report_error("linker_define_func", e);
    wasmrt_functype_delete(ft);

    /* ---- instantiate ------------------------------------------------------------------ */
    wasmrt_store_t *store = wasmrt_store_new(engine);
    CHECK(store != NULL, "store is NULL");

    wasmrt_instance_t inst = {0};
    CHECK(!wasmrt_instance_is_valid(store, inst), "a zero handle must never be valid");

    wasmrt_trap_t *trap = NULL;
    e = wasmrt_linker_instantiate(linker, store, module, &inst, &trap);
    report_error("linker_instantiate", e);
    CHECK(trap == NULL, "unexpected trap while instantiating");
    CHECK(wasmrt_instance_is_valid(store, inst), "the new instance handle must be valid");

    /* A reactor `_initialize` is absent here; that must not be an error. */
    e = wasmrt_instance_initialize(store, inst, &trap);
    report_error("instance_initialize", e);
    CHECK(trap == NULL, "initialize should not trap");

    /* ---- call ------------------------------------------------------------------------- */
    wasmrt_func_t run = {0};
    CHECK(wasmrt_instance_get_func(store, inst, "run", &run), "no export named run");

    wasmrt_functype_t *rt = wasmrt_func_type(store, run);
    CHECK(rt != NULL, "func_type returned NULL");
    if (rt) {
        wasmrt_valkind_t k;
        CHECK(wasmrt_functype_param(rt, 0, &k) && k == WASMRT_I32, "run param 0 should be i32");
        wasmrt_functype_delete(rt);
    }

    wasmrt_val_t args[1];
    args[0].kind = WASMRT_I32;
    args[0].of.i32 = 37;
    wasmrt_val_t results[1];
    e = wasmrt_func_call(store, run, args, 1, results, 1, &trap);
    report_error("func_call", e);
    CHECK(trap == NULL, "run should not trap");
    CHECK(results[0].of.i32 == 42, "expected 37+5=42, got %d", results[0].of.i32);

    /* Arity misuse is an ERROR, not a trap and not a silent truncation. */
    e = wasmrt_func_call(store, run, args, 0, results, 1, &trap);
    CHECK(e != NULL, "a wrong argument count must be an error");
    if (e) { wasmrt_error_delete(e); }

    /* ---- memory ----------------------------------------------------------------------- */
    wasmrt_memory_t mem = {0};
    CHECK(wasmrt_instance_get_memory(store, inst, "memory", &mem), "no export named memory");
    CHECK(wasmrt_memory_size_pages(store, mem) == 1, "expected 1 page");
    CHECK(wasmrt_memory_data_size(store, mem) == 65536, "expected 64 KiB");

    /* The callback wrote "HI" over the data segment's "hi". */
    char seen[2] = {0, 0};
    CHECK(wasmrt_memory_read(store, mem, 0, seen, 2), "memory_read failed");
    CHECK(seen[0] == 'H' && seen[1] == 'I',
          "the host callback's write is not visible: got %c%c", seen[0], seen[1]);

    CHECK(wasmrt_memory_write(store, mem, 4, "ok", 2), "memory_write failed");
    uint8_t *raw = wasmrt_memory_data(store, mem);
    CHECK(raw != NULL, "memory_data returned NULL");
    CHECK(raw && raw[4] == 'o' && raw[5] == 'k', "the raw view disagrees with the checked one");

    /* Out of bounds must be refused, including the offset+n overflow case. */
    CHECK(!wasmrt_memory_read(store, mem, 65535, seen, 2), "an OOB read must be refused");
    CHECK(!wasmrt_memory_read(store, mem, UINT64_MAX, seen, 2), "an overflowing read must be refused");

    /* ---- global ----------------------------------------------------------------------- */
    wasmrt_global_t g = {0};
    CHECK(wasmrt_instance_get_global(store, inst, "answer", &g), "no export named answer");
    wasmrt_val_t gv;
    CHECK(wasmrt_global_get(store, g, &gv), "global_get failed");
    CHECK(gv.kind == WASMRT_I32 && gv.of.i32 == 42, "expected global 42, got %d", gv.of.i32);

    /* ---- trap ------------------------------------------------------------------------- */
    wasmrt_func_t boom = {0};
    CHECK(wasmrt_instance_get_func(store, inst, "boom", &boom), "no export named boom");
    trap = NULL;
    e = wasmrt_func_call(store, boom, NULL, 0, NULL, 0, &trap);
    CHECK(e == NULL, "a guest trap must not be reported as a host error");
    CHECK(trap != NULL, "boom must trap");
    if (trap) {
        const char *m = wasmrt_trap_message(trap);
        CHECK(m != NULL && strlen(m) > 0, "a trap must carry a message");
        printf("  trap message: %s\n", m ? m : "(null)");

        /* ---- backtrace ---------------------------------------------------------------- */
        size_t nframes = wasmrt_trap_frame_count(trap);
        CHECK(nframes >= 1, "a guest trap must report at least the frame it trapped in");

        uint32_t fidx = 0xffffffffu, off = 0xffffffffu;
        const char *fname = (const char *)1; /* not NULL, so we can see it get written */
        CHECK(wasmrt_trap_frame(trap, 0, &fidx, &off, &fname), "frame 0 must be readable");
        printf("  frame 0: func=%u offset=0x%x name=%s\n", fidx, off,
               fname ? fname : "(none)");
        CHECK(fidx != 0xffffffffu, "func_index_out was not written");
        CHECK(off != 0xffffffffu, "offset_out was not written");
        /* A module offset must land inside the module, not at 0 and not past its end. */
        CHECK(off > 0 && off < 4096, "offset 0x%x is not a plausible module offset", off);

        /* Past the end must fail cleanly rather than hand back a stale frame. */
        CHECK(!wasmrt_trap_frame(trap, nframes, NULL, NULL, NULL),
              "reading past the last frame must return false");
        /* Every out-parameter is optional. */
        CHECK(wasmrt_trap_frame(trap, 0, NULL, NULL, NULL),
              "NULL out-parameters must be accepted");

        wasmrt_trap_delete(trap);
    }

    /* A trap a HOST callback raises has no guest stack, and must say so rather than
     * inherit whatever the engine last recorded. */
    {
        wasmrt_trap_t *ht = wasmrt_trap_new("from the host");
        CHECK(ht != NULL, "trap_new failed");
        CHECK(wasmrt_trap_frame_count(ht) == 0, "a host trap must report no wasm frames");
        CHECK(!wasmrt_trap_frame(ht, 0, NULL, NULL, NULL), "a host trap has no frame 0");
        wasmrt_trap_delete(ht);
    }
    CHECK(wasmrt_trap_frame_count(NULL) == 0, "a NULL trap must report zero frames");
    CHECK(!wasmrt_trap_frame(NULL, 0, NULL, NULL, NULL), "a NULL trap must have no frames");

    /* ---- handle checking --------------------------------------------------------------- */
    /* A handle from another store must be rejected, not silently aliased. */
    wasmrt_store_t *other = wasmrt_store_new(engine);
    CHECK(!wasmrt_func_is_valid(other, run), "a foreign func handle must not validate");
    CHECK(!wasmrt_memory_is_valid(other, mem), "a foreign memory handle must not validate");
    trap = NULL;
    e = wasmrt_func_call(other, run, args, 1, results, 1, &trap);
    CHECK(e != NULL, "calling a foreign handle must be an error");
    if (e) { wasmrt_error_delete(e); }
    wasmrt_store_delete(other);

    /* ---- teardown ---------------------------------------------------------------------- */
    /* Deliberately linker-first: an instance still holds the callback, so the environment
     * must outlive it. */
    wasmrt_linker_delete(linker);
    CHECK(env_finalized == 0, "the finalizer must not run while an instance still holds env");
    wasmrt_store_delete(store);
    CHECK(env_finalized == 1, "the finalizer must run once the last holder is gone");

    wasmrt_module_delete(module);
    wasmrt_engine_delete(engine);

    /* Deleting NULL is a no-op everywhere. */
    wasmrt_engine_delete(NULL);
    wasmrt_store_delete(NULL);
    wasmrt_module_delete(NULL);
    wasmrt_trap_delete(NULL);
    wasmrt_error_delete(NULL);

    if (failures == 0) {
        printf("wasmrt c_smoke: OK\n");
        return 0;
    }
    fprintf(stderr, "wasmrt c_smoke: %d failure(s)\n", failures);
    return 1;
}
