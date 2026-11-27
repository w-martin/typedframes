//! A fast non-cryptographic hasher for the checker's internal maps, whose keys are
//! identifiers and offsets from the file being checked. The standard SipHash defends against
//! hostile keys, which costs a measurable share of every run; nothing here is exposed to one.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut words = bytes.chunks_exact(8);
        for chunk in &mut words {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
        let rest = words.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            // The tail is shorter than a word, so its length fits in the spare byte.
            word[7] = rest.len() as u8;
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
pub(crate) type FastSet<T> = HashSet<T, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    #[test]
    fn test_should_behave_as_an_ordinary_map() {
        // arrange
        let mut map: FastMap<(usize, String), u32> = FastMap::default();

        // act
        map.insert((1, "alpha".to_string()), 1);
        map.insert((1, "alphb".to_string()), 2);
        map.insert((2, "alpha".to_string()), 3);

        // assert
        assert_eq!(map.len(), 3);
        assert_eq!(map[&(1, "alphb".to_string())], 2);
        assert_eq!(map.get(&(2, "alpha".to_string())), Some(&3));
        assert!(!map.contains_key(&(3, "alpha".to_string())));
    }

    #[test]
    fn test_should_tell_strings_that_differ_only_in_the_tail_apart() {
        // arrange: hashed the way a map hashes a key.
        let hash = |text: &str| {
            let mut hasher = FxHasher::default();
            text.hash(&mut hasher);
            hasher.finish()
        };

        // act and assert: the final partial word, and a trailing NUL, are told apart.
        assert_ne!(hash("abcdefgh1"), hash("abcdefgh2"));
        assert_ne!(hash("abc"), hash("abd"));
        assert_ne!(hash("a"), hash("a\0"));
    }
}
