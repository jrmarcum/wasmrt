# wazmrt WASI + CLI + pin — Port Map

Files: `src/wasi.zig` (2146 L), `src/main.zig` (520 L), `src/pin.zig` (255 L). Authority: `cmem/security-model.md`.

## 1. WASI preview1 surface — `callFor` name→CallFn table (wasi.zig:678)
Compile-time name→fn map; unknown name → wStubNotsup (errno 58 NOTSUP, still instantiates).
CallFn = fn(ctx:*anyopaque, args:[]const Value, results:[]Value) bool. false=trap(error.HostTrap);
true + results[0]=errno = normal return. All except proc_exit/proc_raise write i32 errno via ret().

Implemented: stdio (fd_write/read/close/sync/datasync[=wFdSync, sync flushes data+meta]);
args/environ (args_sizes_get/args_get/environ_sizes_get/environ_get via writeStringVec);
clocks (clock_time_get, clock_res_get — id0=realtime .real else monotonic .awake, res≥1ns);
random (random_get from std.Random.DefaultPrng seeded from timestamp — NOT crypto; port must decide);
poll_oneoff (partial); proc_exit, proc_raise (trap-unwind), sched_yield (no-op ok), fd_advise (→wSchedYield, advisory);
filesystem (ALL): path_open, fd_seek/tell/pread/pwrite, fd_fdstat_get/set_flags, fd_filestat_get/set_size/set_times,
fd_allocate, fd_readdir, fd_renumber, fd_prestat_get/dir_name, path_filestat_get/set_times,
path_create_directory/remove_directory/unlink_file/rename/link/symlink/readlink.
Stubbed NOTSUP: everything else — all sock_* (sockets unimplemented); fd_fdstat_set_rights NOT in map.

Non-trivial semantics: fd_advise success no-op; fd_allocate = setLength extend-only if offset+len>cur, never
shrink, overflow→EINVAL; fd_filestat_set_times: value bit AND _NOW bit for same ts → EINVAL; fd_readdir NO
cookie seek (restarts walk, skips cookie, O(n²)); cookies 0/1 synthetic ./.. ; dirent 24-byte hdr
{d_next:u64,d_ino:u64,d_namlen:u32,d_type:u8}+name, truncated final entry = correct too-small-buffer signal;
fd_renumber closes to, moves from→to, nulls from, from==to ok; poll_oneoff PARTIAL: clock subs sleep to earliest
(abstime flag 1<<0 subtracts now); fd_read/write subs on valid fd ready immediately (files+stdio never block, no
pipes/sockets); invalid fd → per-event EBADF; fd subs win over clock; nsubs==0→EINVAL; sub 48 bytes, event 32,
tag@+8 (0clock/1fd_read/2fd_write), fd@+16, clock id@+16, timeout@+24, flags@+40.

## 2. Host-fd table & rights
FdEntry = union(enum){stdin,stdout,stderr, dir:Dir, file:File} (wasi.zig:285).
Dir{handle:Io.Dir, preopen_name:?[]const u8 (preopens only, drives fd_prestat_*), rights_base, rights_inheriting
(default rights.all), owned:bool}. File{handle:Io.File, offset:u64 (per-fd WASI offset, independent of host pos;
ALL I/O positional readPositional/writePositional → threadsafe), rights_base, rights_inheriting, flags:u16 (APPEND only)}.
fds: ArrayList(?FdEntry) indexed by guest fd. init seeds 0/1/2; preopens 3+. null=closed. get bounds+null.
put = lowest-free-fd reuse (scan first null else append). deinit closes owned dirs + all files, frees names.
Rights (u64, :133): all=(1<<29)-1. write_mask = fd_write|fd_allocate|path_create_directory|path_create_file|
path_link_source/target|path_rename_source/target|path_filestat_set_times|fd_filestat_set_size|fd_filestat_set_times|
path_remove_directory|path_unlink_file. read_only = all & ~write_mask. Exported: allRights/readOnlyRights.
Per-call: AND-test rights_base & need; fail→ENOTCAPABLE(76). pread/pwrite need (fd_read|fd_write)|fd_seek.
Positional-write honors APPEND by seeking to handle.length() first.
path_open narrowing (:1173): new_base = want_base & rp.inheriting; new_inheriting = want_inheriting & rp.inheriting
(rp.inheriting = PARENT dir fd's rights_inheriting). Guest can only NARROW. --ro-dir carries no write bits in
inheriting → nothing beneath can obtain write, transitively. Create ops gate on rp.inheriting & path_create_file.

## 3. SANDBOX — port exactly
Premise: Io.Dir.resolve_beneath is a silent no-op on Windows & Linux (FreeBSD-only), so *at dir handle is NOT a
boundary. Enforced in userspace, two layers.
Layer 1 lexical resolve() (:256): reject guest path if empty; embedded NUL; absolute (path[0]=='/'||'\\' or
path[1]==':'); a `..` popping empty stack (caught DURING walk so a/../../b caught); component with ':' or =="?"/"??".
Else normalize: split on / AND \, drop . and empties, fold interior .., join with /. Empty→"." (the dir). Result
has no .. left. Symlink targets do NOT go through this (use walk's absolute-rebasing instead).
Layer 2 walkFull() (:509) — RESOLVE_BENEATH in userspace, the crown jewel. Resolves normalized path from start
(preopen handle) → (dir,name,final_is_symlink), following symlinks securely:
- opened: ArrayList(Io.Dir), stack of dir handles; bottom implicitly start (never pushed); topDir = start if empty else last.
- pending components = LIFO (pushReversed: split /+\, push reversed → pop L-to-R). Following symlink pushes target
  components reversed ON TOP → resolve before remainder.
- per component: ./empty skip; `..` → opened empty → ENOTCAPABLE (no handle above preopen, up-escape impossible)
  else pop+close top; else statFile(top,c,.follow_symlinks=false). Missing FINAL + FileNotFound →
  (top,c,final_is_symlink=false) for create. Other stat err → mapped errno.
  If symlink & not(final & !follow_final): budget-- (symlink_max=32; 0→ELOOP); readLink into 4096 buf; if target
  absolute (isAbsoluteTarget: lead /\ or [1]==':') → RESET stack (pop+close all → rebase to preopen root), strip
  leading drive C: then leading seps; pushReversed target. (Absolute = sandbox root, NOT host root; Windows readLink
  may drive-qualify even a /foo link.)
  Final real component → return (top, dup(c), final_is_symlink = kind==.sym_link).
  Intermediate real → must be dir (else ENOTDIR); openDir(top,c,.iterate,.follow_symlinks=false); WINDOWS post-open
  guard (:587): reparse point may open not fail no-follow → fstat handle, if not dir close+ENOTCAPABLE. Push.
- all ./../empty → dir itself, name ".".
Invariant: every open = single component, no-follow, relative to held handle → handle pins inode (TOCTOU-safe vs
intermediate-symlink redirect); .. can't rise above un-pushed start; escaping symlink target fails at follow time.
Security = construction, not lexical inspection.
follow_final per-op: stat/open with SYMLINK_FOLLOW follow final; unlink/readlink/no-follow stat operate on link.
final_is_symlink lets path_open return ELOOP on bare unfollowed symlink (O_NOFOLLOW).
resolveArg() (:626): fd must be .dir; check rights_base&need; slice guest path; resolve() then walkFull(). Returns
ResolvedPath{dir, name(owned), final_is_symlink, opened(handles), inheriting}. Caller MUST rp.close(w).
Residual TOCTOU (:41,path_open:1204): non-create branch resolves final to non-symlink then openFile WITH follow
(no-op, already real) because openFile(.follow_symlinks=false) CRASHES host on Windows (Zig std #18). Narrow final-
component-only window; needs in-sandbox write + race. Intermediate opens have no window. On POSIX a Rust port can
openat(O_NOFOLLOW) and close even this. Platform divergence point.
Platform branches to preserve: Windows reparse post-open fstat (:587); absolute-target drive strip (:573);
path_filestat_set_times opens file + fd-based setTimestamps (Io.Dir.setTimestamps path form @panic TODO on Windows);
avoid nofollow-open on Windows (#18); symlink-creation skips unprivileged Windows; path_symlink (:1303) refuses
ABSOLUTE target at CREATION (defense-in-depth), rejects NUL, stores relative verbatim (containment at follow time).
Mandated adversarial tests to port: symlink-traversal (:1816) + FUZZ (:1902, 2000 iters random symlink graphs, canary
outside preopen NEVER read; oracle = canary content).

## 4. WASI wiring & proc_exit
Wasi.hostFunc(name) (:410) → HostFunc{.native_env={ctx=self, call=callFor(name)}}. Zero interp support needed.
Wasi.memory:?*Memory null at construction; set post-instantiation (main.zig:432 wasi.memory=inst.memory). Memory
helpers readU32/writeU32/readU16/readU64/writeU64/slice return null/false→EFAULT on null-mem or OOB.
iovec/ciovec = {buf:u32,buf_len:u32} (8B); gatherIovecs (:741) maps to host slices into linear memory.
proc_exit unwind: wProcExit sets exit_code, returns false → error.HostTrap. runWasi (main.zig:434) catches
if HostTrap && exit_code!=null → return exit_code (clean exit). proc_raise false no code. Unresolved non-WASI imports
→ unresolvedImport stub returns false → HostTrap.

## 5. CLI (main.zig)
main uses arena + init.io. Modes in order: <2 args→version/usage; args[1]=="pin"→pinSubcommand; read file (≤64MiB),
.wast→wast.runScript print pass/fail/skip, .wat→wat.assemble→wasm, then decode. Exec gate: will_execute =
(args>=3 && findExport(args[2])) || findExport("_start"); if so verifyGate (abort silent on false). Summarize never gated.
Run-export: wazmrt <file> <export> [args] if args[2] names exported func → runFunction (args parsed per param type
i32/i64 parseInt base0, f32/f64 parseFloat; arity checked; runStart then invoke, print results).
WASI command: else _start exported → runWasi, prints (exit N) if nonzero. Else summarize (sections/counts/imports/
exports/per-fn instr+local via decodeBody, validate).
runWasi flags (:365) precede guest argv: --dir <spec>/--ro-dir <spec> (spec=host[:guest], split on LAST : if i>1 else
host=guest; rmask allRights/readOnlyRights; wasi.addPreopen); --env KEY=VAL appended verbatim; --verify <mode>/--pins
<path> consumed (handled by verifyGate); --no-verify/--yes consumed; -- ends wazmrt flags; break on first unrecognized.
argv[0]=path (module path), rest=guest args.
addPreopen (wasi.zig:369): open host dir (openDirAbsolute else rel cwd), .iterate, dup guest_name, append .dir
base=inheriting=dir_rights, return fd.
printTrap (:293): innermost-first frames byte offsets (frameOffset, aligns wasm-objdump) + name-section names.

## 6. pin.zig — Phase 5 IN PROGRESS
Status: pin.zig pure logic COMPLETE + fully tested (hashing/hex/DB parse/mode/decision, 8 tests). Deferred/open:
default policy = .off (owner default-policy question open); signature anchoring mentioned but NOT implemented (only
ownership-anchored plaintext DB); no crypto sig verify, no keys. CLI glue in main.zig (verifyGate/pinSubcommand/
appendPinLine/defaultPinsPath/prompt) present + functional.
hash(bytes)→[32]u8 SHA-256. toHex/hashHex/parseHex (64-char, case-insensitive). TOCTOU: hash the IN-MEMORY buffer
about to execute, never hash-by-path-reopen; main.zig hashes same bytes it decodes+runs. bytes-hashed==bytes-run.
DB (Db, plaintext, content-addressed): one lowercase-hex SHA-256/line; blank + #-lines ignored; trailing text=label.
NO paths in DB (approval by content). contains=linear scan. parse fails LOUD: bad first token → error.InvalidPinLine
→ fail closed. Mode enum off<warn<enforce. modeFromStr, stricter(a,b)=max (dev flag only raises), modeFromDb (reads
`# mode: enforce` directive; inherits DB file ownership). decide(policy,pinned,opt_out,tty)→{run,deny,prompt} pure
tested: if policy==.off||pinned run; if policy==.enforce DENY (before opt_out/tty — no --no-verify/prompt satisfies
enforce); warn+unpinned: opt_out→run, tty→prompt, non-tty→deny.
verifyGate (main.zig:222): DB from --pins or defaultPinsPath (Windows C:\ProgramData\wazmrt\pins else /etc/wazmrt/pins).
Missing DB→.off. Corrupt→fail closed. --verify only raises via stricter. --no-verify/--yes=opt_out. tty=stdin.isTty.
promptYesNo EOF/err→false. pinSubcommand (:133): wazmrt pin <file> [--db <path>] → print "<hex>  <file>"; --db appends
via appendPinLine (RMW whole file). Run with privilege by installer; runtime only reads DB.

Port cautions: (1) walkFull handle-stack IS the boundary — *at handle no-op on Windows+Linux; reimplement userspace
RESOLVE_BENEATH, don't lean on openat2 alone unless accepting Linux-only. (2) Keep absolute-symlink-rebases-to-root
+ ..-can't-pop-below-start exactly. (3) random_get non-crypto PRNG — decide deliberately. (4) enforce-denies-before-
opt_out + DB-fails-closed. (5) per-fd offset + positional-only I/O for threadsafety.
