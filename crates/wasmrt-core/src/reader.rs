//! A zero-copy cursor over a WebAssembly binary: bounds-checked reads and LEB128
//! decoding, with no allocation — the fast, small core the rest of the decoder builds
//! on.
//!
//! Ported from wazmrt `src/Reader.zig` (T1). **Invariant:** the LEB128 readers reject
//! over-long encodings and out-of-range values exactly as the spec (§5.2.2) requires —
//! the 5th-byte `>> 4` / sign-bit checks and the 10th-byte `∈ {0, 0x7f}` checks —
//! because conformance suites probe precisely these edges. See `cmem/design-decisions.md`.

use crate::types::{DecodeError, DecodeResult};

/// A borrowing cursor. Holds a slice of the input and a position; copies nothing.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Start a reader at the beginning of `bytes`.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, pos: 0 }
    }

    /// The current byte offset.
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Bytes left to read.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// True once every byte has been consumed.
    #[must_use]
    pub const fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Read one byte and advance.
    pub fn read_byte(&mut self) -> DecodeResult<u8> {
        if self.pos >= self.bytes.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Read the next byte without advancing (one-byte lookahead).
    pub fn peek_byte(&self) -> DecodeResult<u8> {
        if self.pos >= self.bytes.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        Ok(self.bytes[self.pos])
    }

    /// Borrow `n` bytes from the current position without copying.
    pub fn read_bytes(&mut self, n: usize) -> DecodeResult<&'a [u8]> {
        if self.remaining() < n {
            return Err(DecodeError::UnexpectedEof);
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Read a `vec(…)` length and reject one larger than the bytes that remain: every
    /// element needs ≥1 byte, so a larger count is malformed. Use this (not
    /// [`read_var_u32`]) whenever the count feeds an allocation, so a tiny module can't
    /// force a huge allocation from an untrusted count (OOM amplification).
    ///
    /// [`read_var_u32`]: Reader::read_var_u32
    pub fn read_vec_len(&mut self) -> DecodeResult<u32> {
        let n = self.read_var_u32()?;
        if n as usize > self.remaining() {
            return Err(DecodeError::UnexpectedEof);
        }
        Ok(n)
    }

    /// Read a fixed 32-bit little-endian integer (used for the format version).
    pub fn read_u32_le(&mut self) -> DecodeResult<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read an unsigned LEB128 integer into a `u32` (§5.2.2). Rejects over-long
    /// encodings and values that don't fit in 32 bits ([`DecodeError::LebOverflow`]).
    pub fn read_var_u32(&mut self) -> DecodeResult<u32> {
        let mut result: u32 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            if shift == 28 {
                // 5th byte: only 4 value bits fit, and there must be no 6th byte.
                if byte >> 4 != 0 {
                    return Err(DecodeError::LebOverflow);
                }
                return Ok(result | ((byte as u32) << 28));
            }
            result |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Read an unsigned LEB128 integer into a `u64` (§5.2.2) — for memory64 page counts
    /// and 64-bit indices. Rejects over-long encodings / >64-bit values.
    pub fn read_var_u64(&mut self) -> DecodeResult<u64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            if shift == 63 {
                // 10th byte: only 1 value bit fits, and there must be no 11th byte.
                if byte >> 1 != 0 {
                    return Err(DecodeError::LebOverflow);
                }
                return Ok(result | (((byte as u64) & 0x01) << 63));
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Read a signed LEB128 integer into an `i32` (§5.2.2). Rejects over-long encodings
    /// and values that don't sign-fit in 32 bits.
    pub fn read_var_i32(&mut self) -> DecodeResult<i32> {
        let mut result: u32 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            if shift == 28 {
                // 5th byte: bits 4..6 must sign-extend bit 3 (value bit 31), no 6th byte.
                if byte & 0x80 != 0 {
                    return Err(DecodeError::LebOverflow);
                }
                let hi = byte & 0x78;
                if hi != 0 && hi != 0x78 {
                    return Err(DecodeError::LebOverflow);
                }
                return Ok((result | (((byte & 0x7f) as u32) << 28)) as i32);
            }
            result |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                if byte & 0x40 != 0 {
                    result |= (!0u32) << (shift + 7); // sign-extend
                }
                return Ok(result as i32);
            }
            shift += 7;
        }
    }

    /// Read a signed LEB128 integer into an `i64` (§5.2.2). Rejects over-long encodings
    /// and values that don't sign-fit in 64 bits.
    pub fn read_var_i64(&mut self) -> DecodeResult<i64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            if shift == 63 {
                // 10th byte: only bit 63 fits; bits 1..6 must sign-extend it, no 11th byte.
                if byte & 0x80 != 0 {
                    return Err(DecodeError::LebOverflow);
                }
                let v = byte & 0x7f;
                if v != 0x00 && v != 0x7f {
                    return Err(DecodeError::LebOverflow);
                }
                return Ok((result | (((byte as u64) & 0x01) << 63)) as i64);
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                if byte & 0x40 != 0 {
                    result |= (!0u64) << (shift + 7); // sign-extend
                }
                return Ok(result as i64);
            }
            shift += 7;
        }
    }

    /// Read a signed LEB128 constrained to `s33` — the encoding of block types (§5.3.6)
    /// and heap types (GC). At most 5 bytes; value in `[-2^32, 2^32-1]`. Unlike
    /// [`read_var_i64`], this rejects both over-long (>5-byte) encodings AND values
    /// outside the s33 range: bit 32 is the sign, so e.g. `0x80 0x80 0x80 0x80 0x10`
    /// (2^32) is out of range, not a positive index.
    ///
    /// [`read_var_i64`]: Reader::read_var_i64
    pub fn read_var_s33(&mut self) -> DecodeResult<i64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            if shift == 28 {
                // 5th byte: payload bit 4 (0x10) is value bit 32 = the sign; the higher
                // payload bits (0x60) must sign-extend it, and there is no 6th byte.
                if byte & 0x80 != 0 {
                    return Err(DecodeError::LebOverflow);
                }
                let sign = byte & 0x10;
                let hi = byte & 0x60;
                if (sign == 0 && hi != 0) || (sign != 0 && hi != 0x60) {
                    return Err(DecodeError::LebOverflow);
                }
                let mut r = result | (((byte & 0x1f) as u64) << 28);
                if sign != 0 {
                    r |= (!0u64) << 33; // sign-extend bit 32
                }
                return Ok(r as i64);
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                if byte & 0x40 != 0 {
                    result |= (!0u64) << (shift + 7); // sign-extend
                }
                return Ok(result as i64);
            }
            shift += 7;
        }
    }

    /// Skip a LEB128-encoded integer, consuming bytes until the continuation bit clears.
    /// `max_bytes` bounds the encoding length (5 for a 32-bit LEB, 10 for a 64-bit LEB)
    /// so an over-long encoding is rejected as malformed, not spun on.
    pub fn skip_leb(&mut self, max_bytes: usize) -> DecodeResult<()> {
        let mut n: usize = 0;
        loop {
            let byte = self.read_byte()?;
            n += 1;
            if byte & 0x80 == 0 {
                break;
            }
            if n >= max_bytes {
                return Err(DecodeError::LebOverflow);
            }
        }
        Ok(())
    }

    /// Read a fixed 32-bit little-endian float bit pattern (for `f32.const`).
    pub fn read_f32_bits(&mut self) -> DecodeResult<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a fixed 64-bit little-endian float bit pattern (for `f64.const`).
    pub fn read_f64_bits(&mut self) -> DecodeResult<u64> {
        let b = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_var_i32_decodes_negatives() {
        let mut r = Reader::new(&[0x7f]); // -1
        assert_eq!(r.read_var_i32().unwrap(), -1);
        let mut r2 = Reader::new(&[0x80, 0x7f]); // -128
        assert_eq!(r2.read_var_i32().unwrap(), -128);
    }

    #[test]
    fn read_var_u32_multibyte() {
        let mut r = Reader::new(&[0xE5, 0x8E, 0x26]); // 624485
        assert_eq!(r.read_var_u32().unwrap(), 624485);
        assert!(r.at_end());
    }

    #[test]
    fn read_bytes_past_end_is_eof() {
        let mut r = Reader::new(&[0x00, 0x01]);
        assert_eq!(r.read_bytes(4), Err(DecodeError::UnexpectedEof));
    }

    #[test]
    fn read_var_u32_accepts_5byte_rejects_overlong_and_toobig() {
        let mut ok = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]); // 0xFFFFFFFF
        assert_eq!(ok.read_var_u32().unwrap(), 0xFFFF_FFFF);
        let mut pad = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x00]); // 0, padded to 5 bytes
        assert_eq!(pad.read_var_u32().unwrap(), 0);
        let mut toolong = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]); // 6 bytes
        assert_eq!(toolong.read_var_u32(), Err(DecodeError::LebOverflow));
        let mut toobig = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0x1f]); // 5th byte > 0x0f
        assert_eq!(toobig.read_var_u32(), Err(DecodeError::LebOverflow));
    }

    #[test]
    fn read_var_i64_accepts_10byte_rejects_overlong_and_toobig() {
        let mut zero = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert_eq!(zero.read_var_i64().unwrap(), 0); // 10-byte 0
        let mut neg = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
        assert_eq!(neg.read_var_i64().unwrap(), -1); // 10-byte -1
        let mut toolong =
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert_eq!(toolong.read_var_i64(), Err(DecodeError::LebOverflow)); // 11 bytes
        let mut toobig =
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40]);
        assert_eq!(toobig.read_var_i64(), Err(DecodeError::LebOverflow)); // 10th byte not sign-consistent
    }

    #[test]
    fn read_var_u64_roundtrips() {
        let mut r = Reader::new(&[0xE5, 0x8E, 0x26]); // 624485
        assert_eq!(r.read_var_u64().unwrap(), 624485);
        // full 64-bit max: 9×0xff + 0x01
        let mut max = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
        assert_eq!(max.read_var_u64().unwrap(), u64::MAX);
        // 11th byte → over-long
        let mut toolong =
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert_eq!(toolong.read_var_u64(), Err(DecodeError::LebOverflow));
    }

    #[test]
    fn read_var_s33_range_and_overflow() {
        // 0 and -1 in the short form.
        assert_eq!(Reader::new(&[0x00]).read_var_s33().unwrap(), 0);
        assert_eq!(Reader::new(&[0x7f]).read_var_s33().unwrap(), -1);
        // A positive type index encoded compactly.
        assert_eq!(Reader::new(&[0x08]).read_var_s33().unwrap(), 8);
        // 2^32 (0x80 0x80 0x80 0x80 0x10) is out of s33 range.
        let mut over = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x10]);
        assert_eq!(over.read_var_s33(), Err(DecodeError::LebOverflow));
        // 6 bytes → over-long.
        let mut toolong = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert_eq!(toolong.read_var_s33(), Err(DecodeError::LebOverflow));
    }

    #[test]
    fn fixed_reads() {
        let mut r = Reader::new(&[0x01, 0x00, 0x00, 0x00]);
        assert_eq!(r.read_u32_le().unwrap(), 1);
        assert!(r.at_end());
        // f32 bits little-endian.
        let fbytes = 0x3f80_0000u32.to_le_bytes();
        let mut f = Reader::new(&fbytes);
        assert_eq!(f.read_f32_bits().unwrap(), 0x3f80_0000); // 1.0f32
    }

    #[test]
    fn skip_leb_bounds() {
        let mut r = Reader::new(&[0x80, 0x80, 0x01, 0xAA]); // 3-byte LEB then payload
        r.skip_leb(5).unwrap();
        assert_eq!(r.read_byte().unwrap(), 0xAA);
        let mut over = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert_eq!(over.skip_leb(5), Err(DecodeError::LebOverflow));
    }

    #[test]
    fn read_vec_len_rejects_impossible_count() {
        // Count 100 but only a couple of bytes remain → malformed.
        let mut r = Reader::new(&[0x64, 0x00, 0x00]);
        assert_eq!(r.read_vec_len(), Err(DecodeError::UnexpectedEof));
    }
}
