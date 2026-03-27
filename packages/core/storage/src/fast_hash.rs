use sha2::{Digest, Sha256};

/// Usage: call new with the old hash, a variety of set and remove statements, then call hash to get the new hash.
/// If no set or remove was called, it will return the old hash unmodified.
/// If set or remove are called with keys starting with _, they are ignored ("cheap sidecar data")
/// If you change the data stored or change the order of the operations, the resulting hash will be different
pub struct FastHasher {
    old_hash: Vec<u8>,
    tally: Option<Sha256>,
}

impl FastHasher {
    pub fn new(old_hash: &[u8]) -> Self {
        Self {
            old_hash: old_hash.to_vec(),
            tally: None,
        }
    }

    fn ensure_hasher(&mut self) -> &mut Sha256 {
        if self.tally.is_some() {
            self.tally.as_mut().unwrap()
        } else {
            let mut hasher = Sha256::new();
            hasher.update(&self.old_hash);
            self.tally = Some(hasher);
            self.tally.as_mut().unwrap()
        }
    }

    pub fn set(&mut self, key: &[u8], value: &[u8]) {
        if key[0] != b'_' {
            let hasher = self.ensure_hasher();
            hasher.update(b"set");
            hasher.update(key);
            hasher.update(value);
        }
    }

    pub fn remove(&mut self, key: &[u8]) {
        if key[0] != b'_' {
            let hasher = self.ensure_hasher();
            hasher.update(b"remove");
            hasher.update(key);
        }
    }

    pub fn hash(self) -> Vec<u8> {
        match self.tally {
            Some(hasher) => hasher.finalize().to_vec(),
            None => self.old_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::FastHasher;

    #[test]
    fn reproducable() {
        let old_hash = vec![0u8; 32];
        let mut a = FastHasher::new(&old_hash);
        a.set(b"foo", b"bar");
        a.remove(b"super");
        let hashed_a = a.hash();
        assert_eq!(hashed_a.len(), 32);
        assert_ne!(hashed_a, old_hash);

        let mut b = FastHasher::new(&old_hash);
        b.set(b"foo", b"bar");
        b.remove(b"super");
        let hashed_b = b.hash();
        assert_eq!(hashed_a, hashed_b);
        assert_ne!(hashed_a, old_hash);
    }

    #[test]
    fn empty_block_unchanged() {
        let old_hash = vec![0u8; 32];
        let a = FastHasher::new(&old_hash);
        let hashed_a = a.hash();
        assert_eq!(old_hash, hashed_a);
    }

    #[test]
    fn empty_block_sidecar_ignored() {
        let old_hash = vec![0u8; 32];
        let mut a = FastHasher::new(&old_hash);
        a.set(b"_block_height", b"123");
        a.remove(b"_app_hash");
        let hashed_a = a.hash();
        assert_eq!(old_hash, hashed_a);
    }

    #[test]
    fn full_block_sidecar_ignored() {
        let old_hash = vec![0u8; 32];
        let mut a = FastHasher::new(&old_hash);
        a.set(b"_block_height", b"123");
        a.set(b"important_data", b"must be hashed");
        a.remove(b"_app_hash");
        let hashed_a = a.hash();
        assert_ne!(old_hash, hashed_a);

        let mut b = FastHasher::new(&old_hash);
        b.set(b"important_data", b"must be hashed");
        let hashed_b = b.hash();
        assert_eq!(hashed_a, hashed_b);
    }

    #[test]
    fn different_for_different_data() {
        let mut a = FastHasher::new(&[0u8; 32]);
        a.set(b"foo", b"bar");
        a.remove(b"super");
        let hashed_a = a.hash();
        assert_eq!(hashed_a.len(), 32);

        let mut b = FastHasher::new(&[0u8; 32]);
        b.set(b"foo", b"bard");
        b.remove(b"super");
        let hashed_b = b.hash();
        assert_ne!(hashed_a, hashed_b);

        let mut c = FastHasher::new(&[0u8; 32]);
        c.set(b"fot", b"bar");
        c.remove(b"super");
        let hashed_c = c.hash();
        assert_ne!(hashed_a, hashed_c);

        let mut d = FastHasher::new(&[0u8; 32]);
        d.set(b"foo", b"bar");
        d.remove(b"supes");
        let hashed_d = d.hash();
        assert_ne!(hashed_a, hashed_d);
    }

    #[test]
    fn different_for_different_order() {
        let mut a = FastHasher::new(&[0u8; 32]);
        a.set(b"foo", b"bar");
        a.remove(b"super");
        let hashed_a = a.hash();
        assert_eq!(hashed_a.len(), 32);

        let mut b = FastHasher::new(&[0u8; 32]);
        b.remove(b"super");
        b.set(b"foo", b"bar");
        let hashed_b = b.hash();
        assert_ne!(hashed_a, hashed_b);
    }

    #[test]
    fn different_for_different_start() {
        let mut a = FastHasher::new(&[0u8; 32]);
        a.set(b"foo", b"bar");
        a.remove(b"super");
        let hashed_a = a.hash();
        assert_eq!(hashed_a.len(), 32);

        let mut b = FastHasher::new(&[1u8; 32]);
        b.set(b"foo", b"bar");
        b.remove(b"super");
        let hashed_b = b.hash();
        assert_ne!(hashed_a, hashed_b);
    }

    // FIXME: this would be a nice to have...
    // #[test]
    // fn bacth_size_irrelevant() {
    //     let mut a = FastHasher::new(&[0u8; 32]);
    //     a.set(b"foo", b"bar");
    //     let temp = a.hash();
    //     let mut b = FastHasher::new(&temp);
    //     b.remove(b"super");
    //     let hashed = b.hash();
    //     assert_eq!(hashed.len(), 32);

    //     let mut group = FastHasher::new(&[0u8; 32]);
    //     group.set(b"foo", b"bar");
    //     group.remove(b"super");
    //     let hashed_group = group.hash();
    //     assert_eq!(hashed, hashed_group);
    // }
}
