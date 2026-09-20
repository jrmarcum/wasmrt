#!/usr/bin/env -S deno run --allow-run --allow-read --allow-write --allow-env
// custom-sections-diff.ts — the EXTERNAL gate for what wasmrt's assembler writes OUTSIDE the
// code: custom sections from `@custom`, the `name` section, and branch hints.
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jon Marcum
//
//   deno run -A scripts/custom-sections-diff.ts <wasmrt.exe> <file.wat | @list.txt>...
//   bun scripts/custom-sections-diff.ts <wasmrt.exe> <file.wat | @list.txt>...
//
// Each file is assembled twice — by `wasmrt wat` and by `wasm-tools parse` (the `wat` crate that
// wasmtime reads text with) — and the two outputs are compared on:
//
//   * the ORDER of sections, customs included (placement is the whole meaning of `(after func)`);
//   * every custom section's bytes, exactly;
//   * branch hints by (function, offset PAST THE LOCALS VECTOR, value). The raw offsets are not
//     comparable: wasm-tools packs `(local i32 i32)` as `01 02 7f`, wasmrt as `02 01 7f 01 7f`,
//     and a hint's offset counts from the start of the body. Both are correct for their own body.
//
// Exit status is non-zero if any file differs, so the gate CAN fail — checked by feeding it an
// assembler wrapper that strips the custom sections, which must be reported as a difference.
//
// WHY IT EXISTS: wasmrt's own decoder reads the `name` section leniently and ignores everything
// else in a custom section, so no amount of round-tripping through wasmrt could catch a wrong byte
// here. Agreement between components that learned a format from each other is not evidence
// (`cmem/best-practices.md` §3.8b); only an outside reader is.
//
// 🔻 Ported from `custom-sections-diff.py` (2026-09-19, the TypeScript scripting rule), function
// for function, and verified by running both over the whole `.wat` corpus for the same verdict.
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import process from "node:process";

function uleb(b: Uint8Array, p: number): [number, number] {
  let v = 0, shift = 0;
  for (;;) {
    const x = b[p++];
    v |= (x & 0x7f) << shift;
    if (!(x & 0x80)) return [v >>> 0, p];
    shift += 7;
  }
}

type Section = { id: number; payload: Uint8Array };

function sections(b: Uint8Array): Section[] {
  const out: Section[] = [];
  let p = 8;
  while (p < b.length) {
    const id = b[p];
    const [n, q] = uleb(b, p + 1);
    out.push({ id, payload: b.subarray(q, q + n) });
    p = q + n;
  }
  return out;
}

function customName(payload: Uint8Array): [string, Uint8Array] {
  const [n, q] = uleb(payload, 0);
  return [new TextDecoder().decode(payload.subarray(q, q + n)), payload.subarray(q + n)];
}

function skipValtype(b: Uint8Array, p: number): number {
  const t = b[p++];
  // (ref null? ht) — a heap type: an abstract one is a single byte, a concrete index is a LEB.
  if (t === 0x63 || t === 0x64) [, p] = uleb(b, p);
  return p;
}

/** Length of each body's locals vector, in body order. */
function localsLen(codePayload: Uint8Array): number[] {
  let [count, p] = uleb(codePayload, 0);
  const lens: number[] = [];
  for (let i = 0; i < count; i++) {
    const [size, afterSize] = uleb(codePayload, p);
    const start = afterSize;
    let [groups, q] = uleb(codePayload, start);
    for (let g = 0; g < groups; g++) {
      [, q] = uleb(codePayload, q);
      q = skipValtype(codePayload, q);
    }
    lens.push(q - start);
    p = start + size;
  }
  return lens;
}

type Hint = [number, number, string];

function hints(payload: Uint8Array, firstDef: number, lens: number[]): Hint[] {
  let [n, p] = uleb(payload, 0);
  const out: Hint[] = [];
  for (let i = 0; i < n; i++) {
    let f: number, k: number;
    [f, p] = uleb(payload, p);
    [k, p] = uleb(payload, p);
    for (let j = 0; j < k; j++) {
      let off: number, size: number;
      [off, p] = uleb(payload, p);
      [size, p] = uleb(payload, p);
      const val = payload.subarray(p, p + size);
      p += size;
      out.push([f, off - lens[f - firstDef], hex(val)]);
    }
  }
  return out;
}

function importFuncs(secs: Section[]): number | null {
  for (const { id, payload } of secs) {
    if (id !== 2) continue;
    let [n, p] = uleb(payload, 0);
    let funcs = 0;
    for (let i = 0; i < n; i++) {
      for (let k = 0; k < 2; k++) {
        const [ln, q] = uleb(payload, p);
        p = q + ln;
      }
      const kind = payload[p++];
      if (kind === 0) {
        funcs++;
        [, p] = uleb(payload, p);
      } else {
        return null; // other import kinds: not needed by any hint test
      }
    }
    return funcs;
  }
  return 0;
}

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

type Described = { layout: string[]; customs: [string, string | Hint[]][] };

function describe(b: Uint8Array): Described {
  const secs = sections(b);
  const lens = localsLen(secs.find((s) => s.id === 10)?.payload ?? new Uint8Array([0]));
  const first = importFuncs(secs) ?? 0;
  const layout: string[] = [];
  const customs: [string, string | Hint[]][] = [];
  for (const { id, payload } of secs) {
    if (id === 0) {
      const [name, data] = customName(payload);
      layout.push("custom:" + name);
      customs.push(name === "metadata.code.branch_hint"
        ? [name, hints(data, first, lens)]
        : [name, hex(data)]);
    } else {
      layout.push(String(id));
    }
  }
  return { layout, customs };
}

const assemble = (cmd: string, args: string[]) => {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  return { ok: r.status === 0, err: (r.stderr ?? "").trim() };
};

const wasmrt = resolve(process.argv[2] ?? "");
const files: string[] = [];
// `@list.txt` names a file of paths, one per line — a whole corpus overflows a Windows
// command line.
for (const arg of process.argv.slice(3)) {
  if (arg.startsWith("@")) {
    for (const ln of readFileSync(arg.slice(1), "utf8").split(/\r?\n/)) {
      if (ln.trim()) files.push(ln.trim());
    }
  } else {
    files.push(arg);
  }
}

const wasmTools = spawnSync("wasm-tools", ["--version"], { encoding: "utf8" }).status === 0;
if (!existsSync(wasmrt) || !wasmTools) {
  console.error(`need ${wasmrt} and wasm-tools on PATH`);
  process.exit(2);
}

const tmp = mkdtempSync(join(tmpdir(), "wasmrt-custom-sections-"));
const a = join(tmp, "a.wasm");
const b = join(tmp, "b.wasm");
let bad = 0;
// Agreement that a file is REFUSED compares no bytes at all, so it is counted apart: a probe
// both tools refuse tested nothing, and the headline must not let it pass for a comparison.
const refused: string[] = [];

try {
  for (const f of files) {
    const ra = assemble(wasmrt, ["wat", f, "-o", a]);
    const rb = assemble("wasm-tools", ["parse", f, "-o", b]);
    if (ra.ok !== rb.ok) {
      bad++;
      console.log(`DIFF  ${f}: wasmrt ${ra.ok ? "accepts" : "refuses"}, ` +
        `wasm-tools ${rb.ok ? "accepts" : "refuses"}`);
      console.log(`        wasmrt:     ${ra.err.slice(0, 160)}`);
      console.log(`        wasm-tools: ${rb.err.slice(0, 160)}`);
      continue;
    }
    if (!ra.ok) {
      refused.push(f);
      continue;
    }
    const da = describe(new Uint8Array(readFileSync(a)));
    const db = describe(new Uint8Array(readFileSync(b)));
    if (JSON.stringify(da) !== JSON.stringify(db)) {
      bad++;
      console.log(`DIFF  ${f}`);
      if (JSON.stringify(da.layout) !== JSON.stringify(db.layout)) {
        console.log(`        layout wasmrt:     ${JSON.stringify(da.layout)}`);
        console.log(`        layout wasm-tools: ${JSON.stringify(db.layout)}`);
      }
      for (let i = 0; i < Math.min(da.customs.length, db.customs.length); i++) {
        const [na, xa] = da.customs[i];
        const [nb, xb] = db.customs[i];
        if (na !== nb || JSON.stringify(xa) !== JSON.stringify(xb)) {
          console.log(`        custom '${na}': wasmrt ${JSON.stringify(xa).slice(0, 200)}`);
          console.log(`        custom '${nb}': wasm-tools ${JSON.stringify(xb).slice(0, 200)}`);
        }
      }
    }
  }
} finally {
  rmSync(tmp, { recursive: true, force: true });
}

const compared = files.length - bad - refused.length;
console.log(`custom-sections gate: ${compared} compared byte-for-byte and agree, ` +
  `${refused.length} refused by both, ${bad} differ (of ${files.length})`);
for (const f of refused) console.log(`  refused by both: ${f}`);
process.exit(bad ? 1 : 0);
