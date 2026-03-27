mod length_prefixed;
mod namespace_helpers;

use cosmwasm_std::{Order, Record};
use layer_std::{GasMeter, GasResult};

use crate::{ReadonlyStorage, Storage};
use length_prefixed::{to_length_prefixed, to_length_prefixed_nested};
use namespace_helpers::{concat, prefixed_bounds, trim};

/// An alias of PrefixedStorage::new for less verbose usage
pub fn prefixed<'a>(storage: &'a mut dyn Storage, namespace: &[u8]) -> PrefixedStorage<'a> {
    PrefixedStorage::new(storage, namespace)
}

/// An alias of ReadonlyPrefixedStorage::new for less verbose usage
pub fn prefixed_read<'a>(
    storage: &'a dyn ReadonlyStorage,
    namespace: &[u8],
) -> ReadonlyPrefixedStorage<'a> {
    ReadonlyPrefixedStorage::new(storage, namespace)
}

pub struct PrefixedStorage<'a> {
    storage: &'a mut dyn Storage,
    prefix: Vec<u8>,
}

impl<'a> PrefixedStorage<'a> {
    pub fn new(storage: &'a mut dyn Storage, namespace: &[u8]) -> Self {
        PrefixedStorage {
            storage,
            prefix: to_length_prefixed(namespace),
        }
    }

    // Nested namespaces as documented in
    // https://github.com/webmaster128/key-namespacing#nesting
    pub fn multilevel(storage: &'a mut dyn Storage, namespaces: &[&[u8]]) -> Self {
        PrefixedStorage {
            storage,
            prefix: to_length_prefixed_nested(namespaces),
        }
    }
}

impl<'b> ReadonlyStorage for PrefixedStorage<'b> {
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        self.storage.get(meter, &concat(&self.prefix, key))
    }

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>> {
        let (start, end) = prefixed_bounds(&self.prefix, start, end);

        // get iterator from storage
        let base_iterator = self.storage.range(meter, Some(&start), Some(&end), order)?;

        // make a copy for the closure to handle lifetimes safely
        let mapped = base_iterator.map(move |r| {
            let (k, v) = r?;
            Ok((trim(&self.prefix, &k), v))
        });
        Ok(Box::new(mapped))
    }

    fn abort(self) {}
}

impl<'a> Storage for PrefixedStorage<'a> {
    fn set(&mut self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        self.storage.set(meter, &concat(&self.prefix, key), value)
    }

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        self.storage.remove(meter, &concat(&self.prefix, key))
    }

    fn as_ref(&self) -> &dyn ReadonlyStorage {
        self
    }
}

pub struct ReadonlyPrefixedStorage<'a> {
    storage: &'a dyn ReadonlyStorage,
    prefix: Vec<u8>,
}

impl<'a> ReadonlyPrefixedStorage<'a> {
    pub fn new(storage: &'a dyn ReadonlyStorage, namespace: &[u8]) -> Self {
        ReadonlyPrefixedStorage {
            storage,
            prefix: to_length_prefixed(namespace),
        }
    }

    // Nested namespaces as documented in
    // https://github.com/webmaster128/key-namespacing#nesting
    pub fn multilevel(storage: &'a dyn ReadonlyStorage, namespaces: &[&[u8]]) -> Self {
        ReadonlyPrefixedStorage {
            storage,
            prefix: to_length_prefixed_nested(namespaces),
        }
    }
}

impl<'b> ReadonlyStorage for ReadonlyPrefixedStorage<'b> {
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        self.storage.get(meter, &concat(&self.prefix, key))
    }

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>> {
        let (start, end) = prefixed_bounds(&self.prefix, start, end);

        // get iterator from storage
        let base_iterator = self.storage.range(meter, Some(&start), Some(&end), order)?;

        // make a copy for the closure to handle lifetimes safely
        let mapped = base_iterator.map(move |r| {
            let (k, v) = r?;
            Ok((trim(&self.prefix, &k), v))
        });
        Ok(Box::new(mapped))
    }

    fn abort(self) {}
}

// TODO: enable this
/*
#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::MockStorage;

    #[test]
    fn prefixed_storage_set_and_get() {
        let mut storage = MockStorage::new();

        // set
        let mut s1 = PrefixedStorage::new(&mut storage, b"foo");
        s1.set(b"bar", b"gotcha");
        assert_eq!(storage.get(b"\x00\x03foobar").unwrap(), b"gotcha".to_vec());

        // get
        let s2 = PrefixedStorage::new(&mut storage, b"foo");
        assert_eq!(s2.get(b"bar"), Some(b"gotcha".to_vec()));
        assert_eq!(s2.get(b"elsewhere"), None);
    }

    #[test]
    fn prefixed_storage_multilevel_set_and_get() {
        let mut storage = MockStorage::new();

        // set
        let mut bar = PrefixedStorage::multilevel(&mut storage, &[b"foo", b"bar"]);
        bar.set(b"baz", b"winner");
        assert_eq!(
            storage.get(b"\x00\x03foo\x00\x03barbaz").unwrap(),
            b"winner".to_vec()
        );

        // get
        let bar = PrefixedStorage::multilevel(&mut storage, &[b"foo", b"bar"]);
        assert_eq!(bar.get(b"baz"), Some(b"winner".to_vec()));
        assert_eq!(bar.get(b"elsewhere"), None);
    }

    #[test]
    fn readonly_prefixed_storage_get() {
        let mut storage = MockStorage::new();
        storage.set(b"\x00\x03foobar", b"gotcha");

        // try readonly correctly
        let s1 = ReadonlyPrefixedStorage::new(&storage, b"foo");
        assert_eq!(s1.get(b"bar"), Some(b"gotcha".to_vec()));
        assert_eq!(s1.get(b"elsewhere"), None);

        // no collisions with other prefixes
        let s2 = ReadonlyPrefixedStorage::new(&storage, b"fo");
        assert_eq!(s2.get(b"obar"), None);
    }

    #[test]
    fn readonly_prefixed_storage_multilevel_get() {
        let mut storage = MockStorage::new();
        storage.set(b"\x00\x03foo\x00\x03barbaz", b"winner");

        let bar = ReadonlyPrefixedStorage::multilevel(&storage, &[b"foo", b"bar"]);
        assert_eq!(bar.get(b"baz"), Some(b"winner".to_vec()));
        assert_eq!(bar.get(b"elsewhere"), None);
    }
}
*/
