pub struct BitSet {
    bits: Vec<u64>,
    len: usize,
}

impl BitSet {
    pub fn new(len: usize) -> Self {
        let num_words = (len + 63) / 64;
        Self {
            bits: vec![0u64; num_words],
            len,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn set(&mut self, pos: usize) {
        assert!(pos < self.len);
        self.bits[pos / 64] |= 1u64 << (pos % 64);
    }

    pub fn clear(&mut self, pos: usize) {
        assert!(pos < self.len);
        self.bits[pos / 64] &= !(1u64 << (pos % 64));
    }

    pub fn get(&self, pos: usize) -> bool {
        if pos >= self.len {
            return false;
        }
        (self.bits[pos / 64] >> (pos % 64)) & 1 == 1
    }

    pub fn and(&self, other: &BitSet) -> BitSet {
        let len = self.len.min(other.len);
        let num_words = (len + 63) / 64;
        let mut result = BitSet::new(len);
        for i in 0..num_words {
            result.bits[i] = self.bits[i] & other.bits[i];
        }
        result
    }

    pub fn or(&self, other: &BitSet) -> BitSet {
        let len = self.len.max(other.len);
        let num_words = (len + 63) / 64;
        let mut result = BitSet::new(len);
        for i in 0..num_words {
            let a = self.bits.get(i).copied().unwrap_or(0);
            let b = other.bits.get(i).copied().unwrap_or(0);
            result.bits[i] = a | b;
        }
        result
    }

    pub fn not(&self) -> BitSet {
        let mut result = BitSet::new(self.len);
        for i in 0..self.bits.len() {
            result.bits[i] = !self.bits[i];
        }
        if self.len % 64 != 0 {
            let last = result.bits.len() - 1;
            let mask = (1u64 << (self.len % 64)) - 1;
            result.bits[last] &= mask;
        }
        result
    }

    pub fn count_ones(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    pub fn iter_ones(&self) -> BitSetIter<'_> {
        BitSetIter {
            bitset: self,
            word_idx: 0,
            current_word: if self.bits.is_empty() { 0 } else { self.bits[0] },
            base: 0,
        }
    }

    pub fn raw_words(&self) -> &[u64] {
        &self.bits
    }

    pub fn from_raw_words(words: Vec<u64>, len: usize) -> Self {
        Self { bits: words, len }
    }

    pub fn ensure_capacity(&mut self, new_len: usize) {
        if new_len > self.len {
            let new_words = (new_len + 63) / 64;
            self.bits.resize(new_words, 0);
            self.len = new_len;
        }
    }
}

pub struct BitSetIter<'a> {
    bitset: &'a BitSet,
    word_idx: usize,
    current_word: u64,
    base: usize,
}

impl Iterator for BitSetIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if self.current_word != 0 {
                let tz = self.current_word.trailing_zeros() as usize;
                self.current_word &= self.current_word - 1;
                let pos = self.base + tz;
                if pos < self.bitset.len {
                    return Some(pos);
                } else {
                    return None;
                }
            }
            self.word_idx += 1;
            if self.word_idx >= self.bitset.bits.len() {
                return None;
            }
            self.current_word = self.bitset.bits[self.word_idx];
            self.base = self.word_idx * 64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get() {
        let mut bs = BitSet::new(100);
        assert!(!bs.get(0));
        bs.set(0);
        assert!(bs.get(0));
        bs.set(63);
        bs.set(64);
        bs.set(99);
        assert!(bs.get(63));
        assert!(bs.get(64));
        assert!(bs.get(99));
        assert!(!bs.get(50));
    }

    #[test]
    fn test_clear() {
        let mut bs = BitSet::new(64);
        bs.set(10);
        assert!(bs.get(10));
        bs.clear(10);
        assert!(!bs.get(10));
    }

    #[test]
    fn test_and() {
        let mut a = BitSet::new(128);
        let mut b = BitSet::new(128);
        a.set(1);
        a.set(5);
        a.set(100);
        b.set(5);
        b.set(100);
        b.set(120);
        let result = a.and(&b);
        assert!(!result.get(1));
        assert!(result.get(5));
        assert!(result.get(100));
        assert!(!result.get(120));
    }

    #[test]
    fn test_or() {
        let mut a = BitSet::new(128);
        let mut b = BitSet::new(128);
        a.set(1);
        a.set(5);
        b.set(5);
        b.set(100);
        let result = a.or(&b);
        assert!(result.get(1));
        assert!(result.get(5));
        assert!(result.get(100));
        assert!(!result.get(50));
    }

    #[test]
    fn test_not() {
        let mut bs = BitSet::new(4);
        bs.set(0);
        bs.set(2);
        let inv = bs.not();
        assert!(!inv.get(0));
        assert!(inv.get(1));
        assert!(!inv.get(2));
        assert!(inv.get(3));
    }

    #[test]
    fn test_count_ones() {
        let mut bs = BitSet::new(1000);
        for i in (0..1000).step_by(3) {
            bs.set(i);
        }
        assert_eq!(bs.count_ones(), 334);
    }

    #[test]
    fn test_iter_ones() {
        let mut bs = BitSet::new(200);
        bs.set(0);
        bs.set(63);
        bs.set(64);
        bs.set(199);
        let ones: Vec<usize> = bs.iter_ones().collect();
        assert_eq!(ones, vec![0, 63, 64, 199]);
    }

    #[test]
    fn test_empty_bitset() {
        let bs = BitSet::new(0);
        assert_eq!(bs.count_ones(), 0);
        assert_eq!(bs.iter_ones().count(), 0);
    }
}
