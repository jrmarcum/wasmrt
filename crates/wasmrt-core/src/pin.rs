//! `pin` — module pin verification: a SHA-256 content-addressed allow-list plus the pure
//! [`decide`] policy matrix. **No I/O and no `std`**, so the same logic serves the CLI and the
//! freestanding target; the file reading, the DB location and the prompt live in the CLI.
//!
//! 🔒 **The invariant everything here serves: hash the in-memory bytes you are about to run**
//! (`bytes-hashed == bytes-run`). A digest taken by re-reading the path is a digest of whatever the
//! file says *now*, which is not what is about to execute — see `Loaded` in the CLI, which makes
//! that a type rather than a discipline.
//!
//! **This is a COMPATIBILITY SURFACE** (`cmem/interop.md` §3.3–§3.5): the DB format, the mode
//! ladder, the `decide` precedence and the fail-closed rules must behave identically in wazmrt, so
//! that one root-owned DB governs both runtimes. Changing anything here is a contract change.
//!
//! ⚠️ **Provenance:** wazmrt's *shape*, wasmrt's code — the architecture was read, the implementation
//! is our own (`cmem/security-model.md`, and the same rule the `Store` type registry followed).

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

/// A SHA-256 digest of the bytes that are about to run.
pub type Digest = [u8; 32];

/// Characters in a digest's lowercase-hex spelling.
pub const HEX_LEN: usize = 64;

// ---------------------------------------------------------------------------------------------
// SHA-256
//
// ⚠️ **Written here rather than pulled in, and that needs a reason.** wasmrt has zero third-party
// dependencies as a recorded invariant (`cmem/design-decisions.md`) and Rust's `std` ships no hash,
// so `sha2`/`ring` are out. What makes it acceptable in THIS use: SHA-256 is fully specified with
// public test vectors, and there is **no secret and no key** here — the digests are of public files,
// so there is nothing for a timing side-channel to leak, and the implementation is pinned by the
// NIST vectors below.
//
// 🔒 **The same argument does NOT extend to signatures.** Ed25519 has a secret key, real
// side-channel concerns and malleability rules; hand-rolling it is a different decision, which is
// why the signature path stays design-only (`cmem/security-model.md` §5).
// ---------------------------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4,
    0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe,
    0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f,
    0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da, 0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7,
    0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc,
    0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
    0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070, 0x19a4_c116,
    0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7,
    0xc671_78f2,
];

const H0: [u32; 8] = [
    0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab,
    0x5be0_cd19,
];

/// One 64-byte block through the compression function (FIPS 180-4 §6.2.2).
fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in w.iter_mut().enumerate().take(16) {
        let b = i * 4;
        *word = u32::from_be_bytes([block[b], block[b + 1], block[b + 2], block[b + 3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (dst, src) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *dst = dst.wrapping_add(src);
    }
}

/// SHA-256 of **exactly these bytes**.
#[must_use]
pub fn hash(bytes: &[u8]) -> Digest {
    let mut h = H0;
    let mut chunks = bytes.chunks_exact(64);
    for c in &mut chunks {
        let mut block = [0u8; 64];
        block.copy_from_slice(c);
        compress(&mut h, &block);
    }
    // The tail, its `0x80` terminator, and the 64-bit BIT length — which needs two blocks when the
    // remainder leaves no room for the length field.
    let rest = chunks.remainder();
    let mut block = [0u8; 64];
    block[..rest.len()].copy_from_slice(rest);
    block[rest.len()] = 0x80;
    let bits = (bytes.len() as u64).wrapping_mul(8);
    if rest.len() + 1 + 8 > 64 {
        compress(&mut h, &block);
        block = [0u8; 64];
    }
    block[56..].copy_from_slice(&bits.to_be_bytes());
    compress(&mut h, &block);

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// A digest as lowercase hex — the DB's spelling.
#[must_use]
pub fn to_hex(d: &Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(HEX_LEN);
    for b in d {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// [`hash`] then [`to_hex`].
#[must_use]
pub fn hash_hex(bytes: &[u8]) -> String {
    to_hex(&hash(bytes))
}

/// Parse a digest's hex spelling. Case-insensitive in, and `None` unless it is **exactly** 64 hex
/// characters — a short, long or non-hex token is not "close enough" (see [`Db::parse`]).
#[must_use]
pub fn parse_hex(s: &str) -> Option<Digest> {
    if s.len() != HEX_LEN {
        return None;
    }
    let mut out = [0u8; 32];
    let b = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (b[i * 2] as char).to_digit(16)?;
        let lo = (b[i * 2 + 1] as char).to_digit(16)?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------------------------

/// How strictly an unpinned module is treated. Ordered: `Off < Warn < Enforce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    /// Verification is not applied.
    Off,
    /// An unpinned module warns, prompts on a tty, and is denied without one.
    Warn,
    /// An unpinned module is denied, and **no runtime argument can lower this**.
    Enforce,
}

/// The stricter of two modes — so a runtime flag can only ever **raise** the DB's policy.
#[must_use]
pub fn stricter(a: Mode, b: Mode) -> Mode {
    if a > b {
        a
    } else {
        b
    }
}

/// Parse a mode name. `None` for anything unrecognised; the **caller decides what that means**, and
/// the two callers decide differently on purpose (see [`mode_from_db`] and the CLI's `--verify`).
#[must_use]
pub fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "off" => Some(Mode::Off),
        "warn" => Some(Mode::Warn),
        "enforce" => Some(Mode::Enforce),
        _ => None,
    }
}

/// The `# mode:` directive a DB declares, if it declares one.
///
/// ⚠️⚠️ **FAIL-CLOSED, and this rule is binding (`interop.md` §3.5):** a `# mode:` line that is
/// PRESENT but unrecognised — a typo (`enfroce`), odd capitalisation, a trailing comment — returns
/// **`Enforce`**, never `None`. Degrading it to "no policy" would leave a state that `--no-verify`
/// can override, so **a root-intended enforce would be downgradable by a misspelling**. Absent
/// entirely is the only thing that means "no policy".
#[must_use]
pub fn mode_from_db(text: &str) -> Option<Mode> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim();
        let Some(value) = rest.strip_prefix("mode:") else {
            continue;
        };
        // Exactly one token is the value; anything after it is commentary, and a value that is not
        // one of the three names is a typo — which must fail CLOSED rather than vanish.
        let token = value.split_whitespace().next().unwrap_or("");
        return Some(parse_mode(&token.to_ascii_lowercase()).unwrap_or(Mode::Enforce));
    }
    None
}

/// What the gate does with one module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Execute it.
    Run,
    /// Refuse, and say why.
    Deny,
    /// Ask the user (only reachable with a tty).
    Prompt,
}

/// The whole policy matrix, pure and I/O-free (`interop.md` §3.4 — **this must match wazmrt row for
/// row**, because one root-owned DB governs both).
///
/// * `explicit` — the DB's `# mode:`, if it declares one.
/// * `pinned` — the digest is in the DB.
/// * `opt_out` — `--no-verify` / `--yes`, and **only from the leading run of host flags**: a `--yes`
///   sitting in a guest's argv must never reach this parameter (`interop.md` §2.4).
/// * `tty` — a user is there to answer a prompt.
/// * `armed` — a root key is embedded **or** a pin DB exists. Neither, and there is nothing to
///   verify against, so everything runs: "costs nothing when unarmed" is structural, not promised.
#[must_use]
pub fn decide(explicit: Option<Mode>, pinned: bool, opt_out: bool, tty: bool, armed: bool) -> Action {
    // 1. The DB approved these exact bytes. Nothing else needs asking.
    if pinned {
        return Action::Run;
    }
    match explicit {
        Some(Mode::Off) => Action::Run,
        // 🔒 ABSOLUTE. `opt_out` and `tty` are ignored: the authority is a root-owned file, and a
        // runtime argument must not be able to overrule it. This is the row that makes `enforce`
        // worth deploying.
        Some(Mode::Enforce) => Action::Deny,
        Some(Mode::Warn) => {
            if opt_out {
                Action::Run
            } else if tty {
                Action::Prompt
            } else {
                Action::Deny
            }
        }
        // No policy declared: an unarmed build has nothing to check against, an armed one
        // default-denies, and the user may override on their own machine.
        None if !armed => Action::Run,
        None if opt_out => Action::Run,
        None => Action::Deny,
    }
}

/// A parsed pin DB: the digests it approves.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Db {
    pub entries: Vec<Digest>,
}

/// Why a DB would not parse. A DB that cannot be understood is **never** treated as an empty one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number of the offending line.
    pub line: usize,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "line {}: not a 64-character hex sha256", self.line)
    }
}

impl Db {
    /// Parse the DB text: one lowercase-hex SHA-256 per line, blank and `#` lines ignored, and any
    /// whitespace-separated text after the hash is a human label.
    ///
    /// ⚠️⚠️ **FAIL-LOUD, and this rule is binding (`interop.md` §3.5):** a content line whose first
    /// token is not a valid 64-hex digest is an **error**, not a skipped line. Silently dropping
    /// approvals makes a pinned module look "not in the list" — which reads to the operator as an
    /// attack, and hides the truncated or mangled file that actually caused it.
    ///
    /// # Errors
    /// [`ParseError`] naming the first line that is not a comment, blank, or a digest.
    pub fn parse(text: &str) -> Result<Db, ParseError> {
        let mut entries = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let token = line.split_whitespace().next().unwrap_or("");
            let d = parse_hex(&token.to_ascii_lowercase()).ok_or(ParseError { line: i + 1 })?;
            entries.push(d);
        }
        Ok(Db { entries })
    }

    /// Is this digest approved?
    #[must_use]
    pub fn contains(&self, d: &Digest) -> bool {
        self.entries.iter().any(|e| e == d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The NIST vectors, plus a multi-block input — the only reason it is acceptable to write this
    /// hash by hand is that it is pinned to published answers.
    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            hash_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 56 bytes: the length field does not fit, so this exercises the two-block tail.
        assert_eq!(
            hash_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Exactly one block (64 bytes), where the tail is empty.
        assert_eq!(
            hash_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        // A million 'a' — the classic long vector, which no single-block implementation passes.
        let million = std::vec![b'a'; 1_000_000];
        assert_eq!(
            hash_hex(&million),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn hex_round_trips_and_refuses_anything_that_is_not_a_digest() {
        let d = hash(b"abc");
        assert_eq!(parse_hex(&to_hex(&d)), Some(d));
        assert_eq!(parse_hex(&to_hex(&d).to_uppercase()), Some(d), "case-insensitive IN");
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex(&"a".repeat(63)), None, "too short");
        assert_eq!(parse_hex(&"a".repeat(65)), None, "too long");
        assert_eq!(parse_hex(&"g".repeat(64)), None, "not hex");
    }

    /// ⚠️ The fail-closed rule: a PRESENT but unrecognised mode is `Enforce`, never `None`.
    #[test]
    fn a_mistyped_mode_directive_enforces_rather_than_vanishing() {
        assert_eq!(mode_from_db("# mode: off\n"), Some(Mode::Off));
        assert_eq!(mode_from_db("# mode: warn\n"), Some(Mode::Warn));
        assert_eq!(mode_from_db("# mode: enforce\n"), Some(Mode::Enforce));
        assert_eq!(mode_from_db("#mode:enforce\n"), Some(Mode::Enforce), "no spaces");
        assert_eq!(mode_from_db("# mode: ENFORCE\n"), Some(Mode::Enforce), "case");
        assert_eq!(mode_from_db("# mode: enforce # why\n"), Some(Mode::Enforce), "trailing text");
        // The ones that must NOT become "no policy":
        assert_eq!(mode_from_db("# mode: enfroce\n"), Some(Mode::Enforce), "a typo is enforce");
        assert_eq!(mode_from_db("# mode: \n"), Some(Mode::Enforce), "empty value is enforce");
        assert_eq!(mode_from_db("# mode: 0\n"), Some(Mode::Enforce));
        // Absent entirely is the only "no policy".
        assert_eq!(mode_from_db("# a comment\nabc\n"), None);
        assert_eq!(mode_from_db(""), None);
    }

    #[test]
    fn a_malformed_db_is_an_error_not_an_empty_allow_list() {
        let good = std::format!("{}\n# c\n\n{}  label here\n", hash_hex(b"a"), hash_hex(b"b"));
        let db = Db::parse(&good).expect("parses");
        assert_eq!(db.entries.len(), 2);
        assert!(db.contains(&hash(b"a")) && db.contains(&hash(b"b")));
        assert!(!db.contains(&hash(b"c")));
        // A truncated digest must fail LOUD, naming the line.
        let truncated = std::format!("{}\n{}\n", hash_hex(b"a"), &hash_hex(b"b")[..60]);
        assert_eq!(Db::parse(&truncated), Err(ParseError { line: 2 }));
    }

    /// The `decide` matrix, row for row as `interop.md` §3.4 lists it.
    #[test]
    fn the_decide_matrix_matches_the_contract_row_for_row() {
        use Action::{Deny, Prompt, Run};
        use Mode::{Enforce, Off, Warn};
        // 1. pinned always runs, whatever else is set.
        for explicit in [None, Some(Off), Some(Warn), Some(Enforce)] {
            for opt_out in [false, true] {
                for tty in [false, true] {
                    for armed in [false, true] {
                        assert_eq!(decide(explicit, true, opt_out, tty, armed), Run);
                    }
                }
            }
        }
        assert_eq!(decide(Some(Off), false, false, false, true), Run); // 2
        // 3. enforce denies ABSOLUTELY — opt_out and tty cannot reach it.
        for opt_out in [false, true] {
            for tty in [false, true] {
                assert_eq!(decide(Some(Enforce), false, opt_out, tty, true), Deny);
            }
        }
        assert_eq!(decide(Some(Warn), false, true, false, true), Run); // 4
        assert_eq!(decide(Some(Warn), false, false, true, true), Prompt); // 5
        assert_eq!(decide(Some(Warn), false, false, false, true), Deny); // 6
        assert_eq!(decide(None, false, false, true, false), Run); // 7 — unarmed
        assert_eq!(decide(None, false, true, false, true), Run); // 8
        assert_eq!(decide(None, false, false, false, true), Deny); // 9
    }

    #[test]
    fn a_flag_can_only_raise_the_policy() {
        assert_eq!(stricter(Mode::Off, Mode::Enforce), Mode::Enforce);
        assert_eq!(stricter(Mode::Enforce, Mode::Off), Mode::Enforce);
        assert_eq!(stricter(Mode::Warn, Mode::Off), Mode::Warn);
        assert_eq!(parse_mode("enfroce"), None, "the CLI turns this into an error");
    }
}
