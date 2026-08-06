/*
 * abi_symbols.c — the LINK-COMPLETENESS gate for `wasmrt.h`.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Jon Marcum
 *
 * Takes the address of every function `wasmrt.h` declares and stores it in a table. Taking
 * an address forces the linker to resolve the symbol, so a function that is DECLARED in the
 * header but never EXPORTED from the library fails the build — the failure mode a header
 * cannot otherwise catch, because compiling a declaration always succeeds.
 *
 * `c_smoke.c` proves the functions it calls behave; this proves the ones it does NOT call
 * still exist. Keep it in step with the header: adding a declaration means adding a line
 * here.
 */
#include "wasmrt.h"

#include <stdio.h>

/* Every entry is a distinct function type, so a plain array will not do; casting to a
 * generic function pointer keeps this one table rather than fifty variables, and the cast
 * is enough to force the reference. */
typedef void (*any_fn)(void);

static const any_fn SYMBOLS[] = {
    /* version */
    (any_fn)wasmrt_abi_version,
    (any_fn)wasmrt_version_string,

    /* handle validity */
    (any_fn)wasmrt_instance_is_valid,
    (any_fn)wasmrt_func_is_valid,
    (any_fn)wasmrt_memory_is_valid,
    (any_fn)wasmrt_global_is_valid,

    /* config */
    (any_fn)wasmrt_config_new,
    (any_fn)wasmrt_config_delete,
    (any_fn)wasmrt_config_set_feature,
    (any_fn)wasmrt_config_get_feature,
    (any_fn)wasmrt_config_all_features,
    (any_fn)wasmrt_config_set_max_memory_bytes,
    (any_fn)wasmrt_config_set_max_table_elements,
    (any_fn)wasmrt_config_set_max_gc_objects,
    (any_fn)wasmrt_config_set_max_exception_boxes,
    (any_fn)wasmrt_config_set_max_call_depth,

    /* engine + store */
    (any_fn)wasmrt_engine_new,
    (any_fn)wasmrt_engine_new_with_config,
    (any_fn)wasmrt_engine_delete,
    (any_fn)wasmrt_store_new,
    (any_fn)wasmrt_store_delete,

    /* module */
    (any_fn)wasmrt_module_new,
    (any_fn)wasmrt_module_validate,
    (any_fn)wasmrt_module_delete,
    (any_fn)wasmrt_module_export_count,
    (any_fn)wasmrt_module_export,
    (any_fn)wasmrt_module_import_count,
    (any_fn)wasmrt_module_import,

    /* function types */
    (any_fn)wasmrt_functype_new,
    (any_fn)wasmrt_functype_delete,
    (any_fn)wasmrt_functype_param_count,
    (any_fn)wasmrt_functype_result_count,
    (any_fn)wasmrt_functype_param,
    (any_fn)wasmrt_functype_result,

    /* caller */
    (any_fn)wasmrt_caller_get_memory,
    (any_fn)wasmrt_caller_read,
    (any_fn)wasmrt_caller_write,
    (any_fn)wasmrt_caller_memory_size,

    /* linker */
    (any_fn)wasmrt_linker_new,
    (any_fn)wasmrt_linker_delete,
    (any_fn)wasmrt_linker_define_func,
    (any_fn)wasmrt_linker_define_global,
    (any_fn)wasmrt_linker_define_instance,
    (any_fn)wasmrt_linker_define_wasi,
    (any_fn)wasmrt_linker_define_unknown_imports_as_traps,
    (any_fn)wasmrt_linker_instantiate,

    /* WASI */
    (any_fn)wasmrt_wasi_config_new,
    (any_fn)wasmrt_wasi_config_delete,
    (any_fn)wasmrt_wasi_config_inherit_stdout,
    (any_fn)wasmrt_wasi_config_inherit_stderr,
    (any_fn)wasmrt_wasi_config_inherit_stdin,
    (any_fn)wasmrt_wasi_config_set_args,
    (any_fn)wasmrt_wasi_config_set_env,
    (any_fn)wasmrt_wasi_config_preopen_dir,
    (any_fn)wasmrt_wasi_exit_code,

    /* instance exports */
    (any_fn)wasmrt_instance_get_func,
    (any_fn)wasmrt_instance_get_memory,
    (any_fn)wasmrt_instance_get_global,
    (any_fn)wasmrt_instance_initialize,

    /* calling */
    (any_fn)wasmrt_func_type,
    (any_fn)wasmrt_func_call,

    /* memory */
    (any_fn)wasmrt_memory_data,
    (any_fn)wasmrt_memory_data_size,
    (any_fn)wasmrt_memory_size_pages,
    (any_fn)wasmrt_memory_read,
    (any_fn)wasmrt_memory_write,

    /* globals */
    (any_fn)wasmrt_global_get,

    /* traps + errors */
    (any_fn)wasmrt_trap_new,
    (any_fn)wasmrt_trap_message,
    (any_fn)wasmrt_trap_delete,
    (any_fn)wasmrt_trap_frame_count,
    (any_fn)wasmrt_trap_frame,
    (any_fn)wasmrt_error_message,
    (any_fn)wasmrt_error_delete,
};

int main(void)
{
    const size_t n = sizeof SYMBOLS / sizeof SYMBOLS[0];
    for (size_t i = 0; i < n; i++) {
        if (SYMBOLS[i] == NULL) {
            fprintf(stderr, "wasmrt abi_symbols: entry %zu is NULL\n", i);
            return 1;
        }
    }
    printf("wasmrt abi_symbols: %zu symbols resolved\n", n);
    return 0;
}
