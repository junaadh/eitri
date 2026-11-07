//! SWAR ("SIMD within a register") byte scanning: classify eight source
//! bytes at once with integer arithmetic on a `u64`.
//!
//! Each classifier returns a mask with bit 7 of every matching byte set and
//! all other bits clear. Words are loaded little-endian, so the first byte
//! in memory is the lowest byte and `trailing_zeros / 8` is the index of the
//! first match. Scanners take whole 8-byte words while they fit and finish
//! the tail byte by byte, so short inputs pay nothing extra. All safe Rust:
//! an 8-byte slice-to-array conversion compiles to one load and one bounds
//! check.

/// `0x01` in every byte.
const ONES: u64 = 0x0101_0101_0101_0101;
/// `0x80` in every byte.
const HIGH: u64 = 0x8080_8080_8080_8080;
/// `0x7f` in every byte.
const LOW: u64 = !HIGH;

#[inline(always)]
fn splat(byte: u8) -> u64 {
    ONES * byte as u64
}

#[inline(always)]
fn load(bytes: &[u8], at: usize) -> Option<u64> {
    let word = bytes.get(at..at + 8)?;
    Some(u64::from_le_bytes(word.try_into().unwrap()))
}

/// Bytes equal to zero, exactly: `(b & 0x7f) + 0x7f` sets bit 7 when the
/// low seven bits are not all zero, without carrying into the next byte;
/// OR-ing `b` adds its own bit 7. Bit 7 is then clear only for zero bytes.
#[inline(always)]
fn zero(v: u64) -> u64 {
    !(((v & LOW) + LOW) | v) & HIGH
}

/// Bytes equal to `c`.
#[inline(always)]
fn eq(v: u64, c: u8) -> u64 {
    zero(v ^ splat(c))
}

/// Bytes `>= c`, for bytes and `c` below `0x80`: setting bit 7 before
/// subtracting `c` means no byte borrows from its neighbor, and bit 7
/// survives exactly when the byte was at least `c`.
#[inline(always)]
fn at_least(v: u64, c: u8) -> u64 {
    ((v | HIGH) - splat(c)) & HIGH
}

/// Bytes `<= c`, under the same conditions.
#[inline(always)]
fn at_most(v: u64, c: u8) -> u64 {
    ((splat(c) | HIGH) - v) & HIGH
}

/// Identifier bytes: `[0-9A-Za-z_]`. Bit 7 is stripped before the range
/// tests so none can borrow, and non-ASCII bytes are masked out at the end.
#[inline(always)]
fn ident(v: u64) -> u64 {
    let ascii = !v & HIGH;
    let x = v & LOW;
    let digit = at_least(x, b'0') & at_most(x, b'9');
    // `| 0x20` folds `A-Z` onto `a-z` and maps no other byte into that range.
    let folded = x | splat(0x20);
    let alpha = at_least(folded, b'a') & at_most(folded, b'z');
    (digit | alpha | eq(x, b'_')) & ascii
}

#[inline(always)]
fn first(mask: u64) -> usize {
    (mask.trailing_zeros() / 8) as usize
}

/// End of the run of identifier bytes starting at `at`.
#[inline]
pub(super) fn ident_end(bytes: &[u8], mut at: usize) -> usize {
    while let Some(v) = load(bytes, at) {
        let stop = !ident(v) & HIGH;
        if stop != 0 {
            return at + first(stop);
        }
        at += 8;
    }
    while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
        at += 1;
    }
    at
}

/// End of the run of spaces starting at `at`.
#[inline]
pub(super) fn spaces_end(bytes: &[u8], mut at: usize) -> usize {
    while let Some(v) = load(bytes, at) {
        let stop = !eq(v, b' ') & HIGH;
        if stop != 0 {
            return at + first(stop);
        }
        at += 8;
    }
    while at < bytes.len() && bytes[at] == b' ' {
        at += 1;
    }
    at
}

/// The first `\n` at or after `at`, or the end of input.
#[inline]
pub(super) fn find_newline(bytes: &[u8], mut at: usize) -> usize {
    while let Some(v) = load(bytes, at) {
        let hit = eq(v, b'\n');
        if hit != 0 {
            return at + first(hit);
        }
        at += 8;
    }
    at + bytes[at..]
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len() - at)
}

/// The first byte at or after `at` equal to `x` or `y`, or the end of input.
#[inline]
pub(super) fn find_either(bytes: &[u8], mut at: usize, x: u8, y: u8) -> usize {
    while let Some(v) = load(bytes, at) {
        let hit = eq(v, x) | eq(v, y);
        if hit != 0 {
            return at + first(hit);
        }
        at += 8;
    }
    at + bytes[at..]
        .iter()
        .position(|&b| b == x || b == y)
        .unwrap_or(bytes.len() - at)
}

/// The first byte at or after `at` that can change string-literal state:
/// `"`, `\`, `$`, or a line break; or the end of input.
#[inline]
pub(super) fn find_string_special(bytes: &[u8], mut at: usize) -> usize {
    let special = |b: u8| matches!(b, b'"' | b'\\' | b'$' | b'\n');
    while let Some(v) = load(bytes, at) {
        let hit = eq(v, b'"') | eq(v, b'\\') | eq(v, b'$') | eq(v, b'\n');
        if hit != 0 {
            return at + first(hit);
        }
        at += 8;
    }
    at + bytes[at..]
        .iter()
        .position(|&b| special(b))
        .unwrap_or(bytes.len() - at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The masks agree with scalar definitions for every byte value, in
    /// every lane, next to every neighbor class (borrows would show here).
    #[test]
    fn classifiers_match_scalar_definitions_for_all_bytes() {
        let neighbors = [
            0x00, b' ', b'0', b'9', b'A', b'z', b'_', 0x7f, 0x80, 0xc3, 0xff,
        ];
        for b in 0..=255u8 {
            for &n in &neighbors {
                for lane in 0..8 {
                    let mut word = [n; 8];
                    word[lane] = b;
                    let v = u64::from_le_bytes(word);
                    let bit = |mask: u64| mask >> (lane * 8 + 7) & 1 == 1;
                    assert_eq!(bit(zero(v)), b == 0, "zero {b:#x}");
                    assert_eq!(bit(eq(v, b'"')), b == b'"', "eq {b:#x}");
                    assert_eq!(
                        bit(ident(v)),
                        b.is_ascii_alphanumeric() || b == b'_',
                        "ident {b:#x} next to {n:#x}"
                    );
                    // Every lane's verdict matches its own byte, not its neighbors'.
                    for other in (0..8).filter(|&o| o != lane) {
                        let nb = (ident(v) >> (other * 8 + 7)) & 1 == 1;
                        assert_eq!(nb, n.is_ascii_alphanumeric() || n == b'_');
                    }
                }
            }
        }
    }

    #[test]
    fn scanners_find_the_same_positions_as_byte_loops() {
        let texts = [
            "",
            "a",
            "abc_123 x",
            "abcdefghijklmnopqrstuvwxyz_0123456789(",
            "ab\u{e9}c",
            "        x",
            "  ",
            "x\ny",
            "no newline at all here, long enough",
            "\"q\" \\ $ \n",
            "abcdefgh\"",
            "ident8ch",
            "ident8chx",
        ];
        for text in texts {
            let bytes = text.as_bytes();
            for at in 0..=bytes.len() {
                let scalar = |p: &dyn Fn(u8) -> bool| {
                    at + bytes[at..]
                        .iter()
                        .position(|&b| !p(b))
                        .unwrap_or(bytes.len() - at)
                };
                assert_eq!(
                    ident_end(bytes, at),
                    scalar(&|b| b.is_ascii_alphanumeric() || b == b'_'),
                    "{text:?} @{at}"
                );
                assert_eq!(
                    spaces_end(bytes, at),
                    scalar(&|b| b == b' '),
                    "{text:?} @{at}"
                );
                assert_eq!(
                    find_newline(bytes, at),
                    scalar(&|b| b != b'\n'),
                    "{text:?} @{at}"
                );
                assert_eq!(
                    find_either(bytes, at, b'"', b'\n'),
                    scalar(&|b| b != b'"' && b != b'\n'),
                    "{text:?} @{at}"
                );
                assert_eq!(
                    find_string_special(bytes, at),
                    scalar(&|b| !matches!(b, b'"' | b'\\' | b'$' | b'\n')),
                    "{text:?} @{at}"
                );
            }
        }
    }
}
