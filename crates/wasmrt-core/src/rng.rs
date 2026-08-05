//! `rng` — a ChaCha20 CSPRNG for WASI's `random_get`.
//!
//! **Decision (owner, 2026-08-04):** a ChaCha20 stream seeded once from the OS, matching the
//! frozen oracle (wazmrt moved `random_get` to ChaCha on 2026-07-20, so parity *means* a
//! CSPRNG). Chosen over an OS syscall per call, and over the `getrandom` crate, because it
//! needs **no dependency and no `unsafe`**, is auditable in one file, and still works on the
//! freestanding `wasm32` self-embed target where a syscall has nothing to call.
//!
//! **The rule that matters:** if OS entropy is unavailable, [`ChaCha20Rng::from_os`] returns
//! `None` and the caller fails loudly. A CSPRNG that silently degrades to a fixed seed is
//! worse than none at all, because callers stop checking.
//!
//! ChaCha20 per RFC 8439 §2.3, run as a keystream generator: a 256-bit key, a 96-bit nonce,
//! and a 32-bit block counter. No secret-dependent branches or indexing.

/// The 64-byte ChaCha20 block function state.
type State = [u32; 16];

/// `"expand 32-byte k"` — the RFC 8439 constant, as four little-endian words.
const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// One quarter-round on four state words (RFC 8439 §2.1).
#[inline]
fn quarter_round(s: &mut State, a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// The ChaCha20 block function: 20 rounds (10 double-rounds), then add the input state.
fn block(input: &State) -> [u8; 64] {
    let mut s = *input;
    for _ in 0..10 {
        // Column rounds.
        quarter_round(&mut s, 0, 4, 8, 12);
        quarter_round(&mut s, 1, 5, 9, 13);
        quarter_round(&mut s, 2, 6, 10, 14);
        quarter_round(&mut s, 3, 7, 11, 15);
        // Diagonal rounds.
        quarter_round(&mut s, 0, 5, 10, 15);
        quarter_round(&mut s, 1, 6, 11, 12);
        quarter_round(&mut s, 2, 7, 8, 13);
        quarter_round(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        let w = s[i].wrapping_add(input[i]);
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// A ChaCha20 keystream generator.
pub struct ChaCha20Rng {
    state: State,
    /// The current block's unused tail.
    buf: [u8; 64],
    /// How many bytes of `buf` are still unread; `0` forces a refill.
    avail: usize,
}

impl ChaCha20Rng {
    /// Seed from a 32-byte key and a 12-byte nonce.
    #[must_use]
    pub fn from_seed(key: [u8; 32], nonce: [u8; 12]) -> ChaCha20Rng {
        let mut state: State = [0; 16];
        state[..4].copy_from_slice(&SIGMA);
        for i in 0..8 {
            state[4 + i] = u32::from_le_bytes([
                key[i * 4],
                key[i * 4 + 1],
                key[i * 4 + 2],
                key[i * 4 + 3],
            ]);
        }
        state[12] = 0; // block counter
        for i in 0..3 {
            state[13 + i] = u32::from_le_bytes([
                nonce[i * 4],
                nonce[i * 4 + 1],
                nonce[i * 4 + 2],
                nonce[i * 4 + 3],
            ]);
        }
        ChaCha20Rng {
            state,
            buf: [0; 64],
            avail: 0,
        }
    }

    /// Seed from the operating system's entropy source.
    ///
    /// Returns `None` if the OS will not provide entropy. **Callers must fail on `None`**
    /// rather than substituting a fixed seed — see the module docs.
    #[cfg(feature = "std")]
    #[must_use]
    pub fn from_os() -> Option<ChaCha20Rng> {
        let mut seed = [0u8; 44];
        os_entropy(&mut seed)?;
        let mut key = [0u8; 32];
        let mut nonce = [0u8; 12];
        key.copy_from_slice(&seed[..32]);
        nonce.copy_from_slice(&seed[32..]);
        Some(ChaCha20Rng::from_seed(key, nonce))
    }

    /// Fill `dst` with keystream bytes.
    pub fn fill(&mut self, dst: &mut [u8]) {
        let mut n = 0;
        while n < dst.len() {
            if self.avail == 0 {
                self.buf = block(&self.state);
                // 2^32 blocks (256 GiB) per nonce is the ChaCha20 limit; wrapping past it
                // would repeat keystream, so re-seeding is the caller's job long before.
                self.state[12] = self.state[12].wrapping_add(1);
                self.avail = 64;
            }
            let take = core::cmp::min(self.avail, dst.len() - n);
            let from = 64 - self.avail;
            dst[n..n + take].copy_from_slice(&self.buf[from..from + take]);
            self.avail -= take;
            n += take;
        }
    }
}

/// Read `dst.len()` bytes from the OS entropy source. `None` if unavailable.
///
/// Uses only what `std` already exposes, so there is no dependency and no `unsafe`: on
/// Unix-likes `/dev/urandom` is the documented interface; on Windows it comes from the OS
/// via `std`'s own `RandomState` hasher keys, which are seeded from `BCryptGenRandom`.
#[cfg(feature = "std")]
fn os_entropy(dst: &mut [u8]) -> Option<()> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom").ok()?;
        f.read_exact(dst).ok()?;
        return Some(());
    }
    #[cfg(not(unix))]
    {
        // `RandomState` is seeded from the OS CSPRNG (`BCryptGenRandom` on Windows) and is
        // re-keyed per instance. Hashing a counter under a fresh key per 8 bytes gives an
        // OS-seeded, non-predictable fill without a dependency or `unsafe`.
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut i = 0usize;
        while i < dst.len() {
            let mut h = RandomState::new().build_hasher();
            h.write_usize(i);
            let v = h.finish().to_le_bytes();
            let take = core::cmp::min(8, dst.len() - i);
            dst[i..i + take].copy_from_slice(&v[..take]);
            i += take;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8439 §2.3.2 test vector: the block function's output for the specified state.
    #[test]
    fn matches_the_rfc8439_block_vector() {
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce = [0, 0, 0, 9, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let mut r = ChaCha20Rng::from_seed(key, nonce);
        r.state[12] = 1; // the vector fixes the counter at 1
        let out = block(&r.state);
        // First 16 bytes of the RFC's expected serialized block.
        assert_eq!(
            &out[..16],
            &[
                0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3,
                0x20, 0x71, 0xc4
            ]
        );
    }

    #[test]
    fn is_deterministic_for_a_fixed_seed() {
        let mk = || ChaCha20Rng::from_seed([7u8; 32], [3u8; 12]);
        let (mut a, mut b) = (mk(), mk());
        let (mut x, mut y) = ([0u8; 200], [0u8; 200]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn fills_across_block_boundaries_without_repeating() {
        // 64 bytes is exactly one block; a naive implementation that forgets to bump the
        // counter would repeat the first block forever.
        let mut r = ChaCha20Rng::from_seed([1u8; 32], [2u8; 12]);
        let mut buf = [0u8; 192];
        r.fill(&mut buf);
        assert_ne!(&buf[..64], &buf[64..128]);
        assert_ne!(&buf[64..128], &buf[128..]);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        // The partial-buffer path must produce the same stream as a single fill.
        let mut a = ChaCha20Rng::from_seed([9u8; 32], [4u8; 12]);
        let mut b = ChaCha20Rng::from_seed([9u8; 32], [4u8; 12]);
        let mut whole = [0u8; 130];
        a.fill(&mut whole);
        let mut piece = [0u8; 130];
        for chunk in piece.chunks_mut(7) {
            b.fill(chunk);
        }
        assert_eq!(whole, piece);
    }

    #[test]
    #[cfg(feature = "std")]
    fn os_seeding_produces_distinct_streams() {
        let mut a = ChaCha20Rng::from_os().expect("OS entropy unavailable");
        let mut b = ChaCha20Rng::from_os().expect("OS entropy unavailable");
        let (mut x, mut y) = ([0u8; 64], [0u8; 64]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_ne!(x, y, "two OS-seeded streams must differ");
        assert_ne!(x, [0u8; 64], "must not be all zeroes");
    }
}
