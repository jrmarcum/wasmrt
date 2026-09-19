#!/usr/bin/env python
"""custom-sections-diff.py — the EXTERNAL gate for what wasmrt's assembler writes OUTSIDE the
code: custom sections from `@custom`, the `name` section, and branch hints.

    python scripts/custom-sections-diff.py <wasmrt.exe> <file.wat | @list.txt>...

Each file is assembled twice — by `wasmrt wat` and by `wasm-tools parse` (the `wat` crate that
wasmtime reads text with) — and the two outputs are compared on:

  * the ORDER of sections, customs included (placement is the whole meaning of `(after func)`);
  * every custom section's bytes, exactly;
  * branch hints by (function, offset PAST THE LOCALS VECTOR, value). The raw offsets are not
    comparable: wasm-tools packs `(local i32 i32)` as `01 02 7f`, wasmrt as `02 01 7f 01 7f`,
    and a hint's offset counts from the start of the body. Both are correct for their own body.

Exit status is non-zero if any file differs, so the gate CAN fail — checked by feeding it a
module whose placement was deliberately wrong when it was written.

WHY IT EXISTS: wasmrt's own decoder reads the `name` section leniently and ignores everything
else in a custom section, so no amount of round-tripping through wasmrt could catch a wrong byte
here. Agreement between components that learned a format from each other is not evidence
(`cmem/best-practices.md` §3.8b); only an outside reader is.
"""
import subprocess
import sys
import tempfile
import os
import shutil


def uleb(b, p):
    v = shift = 0
    while True:
        x = b[p]
        p += 1
        v |= (x & 0x7F) << shift
        if not x & 0x80:
            return v, p
        shift += 7


def sections(b):
    out, p = [], 8
    while p < len(b):
        sid = b[p]
        n, q = uleb(b, p + 1)
        out.append((sid, b[q:q + n]))
        p = q + n
    return out


def custom_name(payload):
    n, q = uleb(payload, 0)
    return payload[q:q + n].decode("utf-8", "replace"), payload[q + n:]


def locals_len(code_payload):
    """Length of each body's locals vector, in body order."""
    count, p = uleb(code_payload, 0)
    lens = []
    for _ in range(count):
        size, p = uleb(code_payload, p)
        start = p
        groups, q = uleb(code_payload, p)
        for _ in range(groups):
            _, q = uleb(code_payload, q)
            q = skip_valtype(code_payload, q)
        lens.append(q - start)
        p = start + size
    return lens


def skip_valtype(b, p):
    t = b[p]
    p += 1
    if t in (0x63, 0x64):  # (ref null? ht)
        # heap type: a negative s33 abstract type is one byte; a concrete index is a LEB
        _, p = uleb(b, p)
    return p


def hints(payload, first_def, lens):
    n, p = uleb(payload, 0)
    out = []
    for _ in range(n):
        f, p = uleb(payload, p)
        k, p = uleb(payload, p)
        for _ in range(k):
            off, p = uleb(payload, p)
            size, p = uleb(payload, p)
            val = payload[p:p + size]
            p += size
            out.append((f, off - lens[f - first_def], bytes(val)))
    return out


def import_funcs(secs):
    for sid, pl in secs:
        if sid == 2:
            n, p = uleb(pl, 0)
            funcs = 0
            for _ in range(n):
                for _ in range(2):
                    ln, p = uleb(pl, p)
                    p += ln
                kind = pl[p]
                p += 1
                if kind == 0:
                    funcs += 1
                    _, p = uleb(pl, p)
                else:
                    return None  # other import kinds: not needed by any hint test
            return funcs
    return 0


def describe(b):
    secs = sections(b)
    lens = next((locals_len(pl) for sid, pl in secs if sid == 10), [])
    first = import_funcs(secs) or 0
    layout, customs = [], []
    for sid, pl in secs:
        if sid == 0:
            name, data = custom_name(pl)
            layout.append("custom:" + name)
            if name == "metadata.code.branch_hint":
                customs.append((name, hints(data, first, lens)))
            else:
                customs.append((name, bytes(data)))
        else:
            layout.append(str(sid))
    return layout, customs


def assemble(cmd):
    r = subprocess.run(cmd, capture_output=True)
    return r.returncode == 0, r.stderr.decode("utf-8", "replace").strip()


def main():
    wasmrt, files = os.path.abspath(sys.argv[1]), []
    # `@list.txt` names a file of paths, one per line — a whole corpus overflows a Windows
    # command line.
    for arg in sys.argv[2:]:
        if arg.startswith("@"):
            files += [ln.strip() for ln in open(arg[1:], encoding="utf-8") if ln.strip()]
        else:
            files.append(arg)
    wasm_tools = shutil.which("wasm-tools")
    if not os.path.exists(wasmrt) or not wasm_tools:
        sys.exit(f"need {wasmrt} and wasm-tools on PATH (found: {wasm_tools})")
    bad = 0
    # Agreement that a file is REFUSED compares no bytes at all, so it is counted apart: a probe
    # both tools refuse tested nothing, and the headline must not let it pass for a comparison.
    refused = []
    tmp = tempfile.mkdtemp()
    a, b = os.path.join(tmp, "a.wasm"), os.path.join(tmp, "b.wasm")
    for f in files:
        ok_a, err_a = assemble([wasmrt, "wat", f, "-o", a])
        ok_b, err_b = assemble([wasm_tools, "parse", f, "-o", b])
        if ok_a != ok_b:
            bad += 1
            print(f"DIFF  {f}: wasmrt {'accepts' if ok_a else 'refuses'}, "
                  f"wasm-tools {'accepts' if ok_b else 'refuses'}")
            print(f"        wasmrt:     {err_a[:160]}")
            print(f"        wasm-tools: {err_b[:160]}")
            continue
        if not ok_a:
            refused.append(f)
            continue
        da, db = describe(open(a, "rb").read()), describe(open(b, "rb").read())
        if da != db:
            bad += 1
            print(f"DIFF  {f}")
            if da[0] != db[0]:
                print(f"        layout wasmrt:     {da[0]}")
                print(f"        layout wasm-tools: {db[0]}")
            for (na, xa), (nb, xb) in zip(da[1], db[1]):
                if (na, xa) != (nb, xb):
                    print(f"        custom {na!r}: wasmrt {xa!r:.200}")
                    print(f"        custom {nb!r}: wasm-tools {xb!r:.200}")
    compared = len(files) - bad - len(refused)
    print(f"custom-sections gate: {compared} compared byte-for-byte and agree, "
          f"{len(refused)} refused by both, {bad} differ (of {len(files)})")
    for f in refused:
        print(f"  refused by both: {f}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
