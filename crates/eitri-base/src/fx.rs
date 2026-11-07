//! The Fx hash (from Firefox and rustc): one multiply per word. Fast for the
//! short keys a compiler hashes (spellings, ids) and adequate because the
//! inputs are not adversarial hash-flooding targets: collisions cost probe
//! length, never correctness, since every lookup compares the key.

use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    const K: u64 = 0xf135_7aea_2e62_a9c5;

    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash ^ word).wrapping_mul(Self::K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while let Some((word, rest)) = bytes.split_first_chunk::<8>() {
            self.add(u64::from_le_bytes(*word));
            bytes = rest;
        }
        if let Some((word, rest)) = bytes.split_first_chunk::<4>() {
            self.add(u32::from_le_bytes(*word) as u64);
            bytes = rest;
        }
        for &byte in bytes {
            self.add(byte as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // The multiply leaves the best-mixed bits on top; rotate them down
        // to where power-of-two tables take their index from.
        self.hash.rotate_left(26)
    }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;
pub type FxHashSet<K> = std::collections::HashSet<K, FxBuildHasher>;

/// Hash a byte string, with its length mixed in so prefixes differ.
#[inline]
pub fn fx_hash(bytes: &[u8]) -> u64 {
    let mut hasher = FxHasher::default();
    hasher.write(bytes);
    hasher.write_usize(bytes.len());
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_deterministic_and_length_sensitive() {
        assert_eq!(fx_hash(b"while"), fx_hash(b"while"));
        assert_ne!(fx_hash(b"a"), fx_hash(b"a\0"));
        assert_ne!(fx_hash(b""), fx_hash(b"\0"));
        let mut map = FxHashMap::default();
        map.insert("x", 1);
        assert_eq!(map["x"], 1);
    }
}
