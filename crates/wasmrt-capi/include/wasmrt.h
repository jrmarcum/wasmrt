/*
 * wasmrt.h — the public C ABI for the wasmrt WebAssembly runtime.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Jon Marcum
 *
 * STATUS: T0 scaffold. This header is DESIGNED (not transliterated from wasm.h) at task T8
 * — see cmem/roadmap.md and docs/port/wasmrt.h.draft. The full ~40-fn surface (lightweight
 * {id} handles into a store, caller-based host callbacks) is finalized with the owner at the
 * start of T8. Only the version handshake exists so far.
 */
#ifndef WASMRT_H
#define WASMRT_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>

/* The wasmrt C-ABI version — bumped on any breaking change to this header. */
uint32_t wasmrt_abi_version(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* WASMRT_H */
