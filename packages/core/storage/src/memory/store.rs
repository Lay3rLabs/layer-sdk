use futures::stream::{iter, Stream};
use parking_lot::{RwLock, RwLockReadGuard};
use std::collections::BTreeMap;
use std::fmt;
use std::iter;
use std::ops::{Bound, RangeBounds};
use std::pin::Pin;
use tracing::{debug_span, trace_span};

use cosmwasm_std::{Order, Record};
use layer_std::{GasMeter, GasResult, HexEncode};

use crate::prices::PriceList;
use crate::traits::BatchChanges;
use crate::traits::Transaction;
use crate::traits::KV;
use crate::wrap::{Op, ReaderWrapper};
use crate::SyncableStorage;
use crate::DEFAULT_PERSISTED_PRICES;
use crate::{FastHasher, PersistentStorage, ReadonlyStorage, Storage};

pub struct MemoryStore(RwLock<BTreeStorage>);

impl MemoryStore {
    pub fn new() -> Self {
        MemoryStore(RwLock::new(BTreeStorage::new()))
    }

    /// This is only meant for testing as a way to "Clone" a DB from one app to another
    pub fn import(src: &dyn ReadonlyStorage, meter: Option<&GasMeter>) -> GasResult<Self> {
        let inf = GasMeter::infinite();
        let meter = meter.unwrap_or(&inf);
        let btree = BTreeStorage::import(src, meter)?;
        Ok(MemoryStore(RwLock::new(btree)))
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemoryStore with {} elements", self.0.read().data.len())
    }
}

struct BTreeStorage {
    hash: Vec<u8>,
    data: BTreeMap<Vec<u8>, Vec<u8>>,
    price_list: PriceList,
}

impl PersistentStorage for MemoryStore {
    type Reader<'a> = MemoryStorageReader<'a>;

    type Writer<'a> = MemoryStorageWriter<'a>;

    fn reader(&self) -> MemoryStorageReader<'_> {
        let reader = self.0.read();
        MemoryStorageReader(reader)
    }

    fn writer(&self) -> MemoryStorageWriter<'_> {
        MemoryStorageWriter::new(self)
    }

    fn app_hash(&self) -> Vec<u8> {
        self.0.read().hash.clone()
    }
}

fn iter_stream<I: Iterator + Send + 'static>(it: I) -> Pin<Box<dyn Stream<Item = I::Item> + Send>> {
    Box::pin(iter(it))
}

impl SyncableStorage for MemoryStore {
    // TODO: This one is not implemented. Only needed for state sync
    fn latest_sequence(&self) -> u64 {
        2
    }

    // This one is implemented
    fn current_state(&self) -> Pin<Box<dyn Stream<Item = Result<KV, String>> + Send>> {
        let inner = self.0.read();
        let it = inner.data.iter().map(|(k, v)| (k.to_owned(), v.to_owned()));
        let data: Vec<_> = it.collect();
        iter_stream(data.into_iter().map(Ok))
    }

    // TODO: This one is not implemented. Only needed for state sync
    fn changes_since(
        &self,
        _sequence: u64,
    ) -> Pin<Box<dyn Stream<Item = Result<BatchChanges, String>> + Send>> {
        iter_stream([].into_iter())
    }
}

pub struct MemoryStorageReader<'a>(RwLockReadGuard<'a, BTreeStorage>);

impl ReadonlyStorage for MemoryStorageReader<'_> {
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let _span = trace_span!("get", key = %HexEncode::new(&key)).entered();
        self.0.get(meter, key)
    }

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>> {
        let _span = trace_span!("range").entered();
        self.0.range(meter, start, end, order)
    }

    fn abort(self) {}
}

pub struct MemoryStorageWriter<'a> {
    // This is needed for commit later on
    persistent: &'a MemoryStore,
    // This is the reader access
    reader: MemoryStorageReader<'a>,
    // This is a wrapper used for transactions
    wrapper: ReaderWrapper,
}

impl<'a> MemoryStorageWriter<'a> {
    fn new(persistent: &'a MemoryStore) -> Self {
        let reader = persistent.reader();
        Self {
            persistent,
            reader,
            wrapper: ReaderWrapper::new(),
        }
    }
}

impl ReadonlyStorage for MemoryStorageWriter<'_> {
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let _span = trace_span!("get", key = %HexEncode::new(&key)).entered();
        self.wrapper.get(&self.reader, meter, key)
    }

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>> {
        let _span = trace_span!("range").entered();
        self.wrapper.range(&self.reader, meter, start, end, order)
    }

    fn abort(self) {}
}

impl Storage for MemoryStorageWriter<'_> {
    fn set(&mut self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        let _span =
            trace_span!("set", key = %HexEncode::new(&key), value = %HexEncode::new(&value))
                .entered();
        self.wrapper.set(meter, key, value)
    }

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        let _span = trace_span!("remove", key = %HexEncode::new(&key)).entered();
        self.wrapper.remove(meter, key)
    }

    fn as_ref(&self) -> &dyn ReadonlyStorage {
        self
    }
}

impl Transaction for MemoryStorageWriter<'_> {
    fn commit(self, meter: &GasMeter) -> GasResult<()> {
        let _span = debug_span!("commit", db = "memory",).entered();
        // destructure and force dropping reader to remove read lock (otherwise, deadlock on getting writer below)
        // println!(
        //     "lock status: {}, exclusive: {}",
        //     self.persistent.0.is_locked(),
        //     self.persistent.0.is_locked_exclusive()
        // );
        self.reader.abort();
        // println!(
        //     "lock status: {}, exclusive: {}",
        //     self.persistent.0.is_locked(),
        //     self.persistent.0.is_locked_exclusive()
        // );
        let mut hasher = FastHasher::new(&self.persistent.0.read().hash);

        // write all operations to the root storage
        let ops = self.wrapper.prepare();
        let mut writer = self.persistent.0.write();
        for op in ops {
            match op {
                Op::Set { key, value } => {
                    hasher.set(&key, &value);
                    writer.set(meter, key, value)?;
                }
                Op::Delete { key } => {
                    hasher.remove(&key);
                    writer.remove(meter, &key)?;
                }
            }
        }

        // calculate new app hash
        writer.hash = hasher.hash();
        Ok(())
    }

    fn as_mut(&mut self) -> &mut dyn Storage {
        self
    }
}

impl BTreeStorage {
    pub fn new() -> Self {
        BTreeStorage {
            hash: vec![0; 32],
            data: BTreeMap::new(),
            price_list: DEFAULT_PERSISTED_PRICES,
        }
    }
}

impl Default for BTreeStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl BTreeStorage {
    // Make a copy of another storage
    fn import(source: &dyn ReadonlyStorage, meter: &GasMeter) -> GasResult<Self> {
        let mut storage = BTreeStorage::new();
        let keys = source.range(meter, None, None, Order::Ascending)?;
        for r in keys {
            let (key, value) = r?;
            storage.set(meter, key, value).unwrap();
        }
        Ok(storage)
    }

    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let value = self.data.get(key).cloned();

        let val_len = value.as_deref();
        self.price_list.charge_read(meter, key, val_len)?;
        Ok(value)
    }

    /// range allows iteration over a set of keys, either forwards or backwards
    /// uses standard rust range notation, and eg db.range(b"foo"..b"bar") also works reverse
    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>> {
        let bounds = range_bounds(start, end);

        // BTreeMap.range panics if range is start > end.
        // However, this cases represent just empty range and we treat it as such.
        match (bounds.start_bound(), bounds.end_bound()) {
            (Bound::Included(start), Bound::Excluded(end)) if start > end => {
                return Ok(Box::new(iter::empty()));
            }
            _ => {}
        }

        self.price_list.charge_range(meter)?;

        // FIXME: wrap this so we can instrument next to for timing
        let iter = self
            .data
            .range(bounds)
            .map(|(key, value)| -> GasResult<BTreeMapRecordRef> {
                self.price_list
                    .charge_read(meter, key, Some(value.as_slice()))?;
                Ok((key, value))
            });
        match order {
            Order::Ascending => Ok(Box::new(iter.map(clone_item))),
            Order::Descending => Ok(Box::new(iter.rev().map(clone_item))),
        }
    }

    fn set(&mut self, meter: &GasMeter, key: Vec<u8>, value: Vec<u8>) -> GasResult<()> {
        // TODO: review this, I think panic is quite sketchy
        // if value.is_empty() {
        //     panic!("TL;DR: Value must not be empty in Storage::set but in most cases you can use Storage::remove instead. Long story: Getting empty values from storage is not well supported at the moment. Some of our internal interfaces cannot differentiate between a non-existent key and an empty value. Right now, you cannot rely on the behaviour of empty values. To protect you from trouble later on, we stop here. Sorry for the inconvenience! We highly welcome you to contribute to CosmWasm, making this more solid one way or the other.");
        // }
        self.price_list.charge_write(meter, &key, &value)?;
        self.data.insert(key, value);
        Ok(())
    }

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        self.price_list.charge_remove(meter, key)?;
        self.data.remove(key);
        Ok(())
    }
}

/// This debug implementation is made for inspecting storages in unit testing.
/// It is made for human readability only and the output can change at any time.
impl fmt::Debug for BTreeStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemoryStorage ({} entries)", self.data.len())?;
        f.write_str(" {\n")?;
        for (key, value) in &self.data {
            f.write_str("  0x")?;
            for byte in key {
                write!(f, "{:02x}", byte)?;
            }
            f.write_str(": 0x")?;
            for byte in value {
                write!(f, "{:02x}", byte)?;
            }
            f.write_str("\n")?;
        }
        f.write_str("}")?;
        Ok(())
    }
}

fn range_bounds(start: Option<&[u8]>, end: Option<&[u8]>) -> impl RangeBounds<Vec<u8>> {
    (
        start.map_or(Bound::Unbounded, |x| Bound::Included(x.to_vec())),
        end.map_or(Bound::Unbounded, |x| Bound::Excluded(x.to_vec())),
    )
}

/// The BTreeMap specific key-value pair reference type, as returned by BTreeMap<Vec<u8>, Vec<u8>>::range.
/// This is internal as it can change any time if the map implementation is swapped out.
type BTreeMapRecordRef<'a> = (&'a Vec<u8>, &'a Vec<u8>);

fn clone_item(item_ref: GasResult<BTreeMapRecordRef>) -> GasResult<Record> {
    item_ref.map(|(key, value)| (key.clone(), value.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_std::GasError;

    #[test]
    fn get_and_set() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas = GasMeter::infinite();
        assert_eq!(store.get(&gas, b"foo").unwrap(), None);
        store.set(&gas, b"foo", b"bar").unwrap();
        assert_eq!(store.get(&gas, b"foo").unwrap(), Some(b"bar".to_vec()));
        assert_eq!(store.get(&gas, b"food").unwrap(), None);
    }

    // #[test]
    // #[should_panic(
    //     expected = "Getting empty values from storage is not well supported at the moment."
    // )]
    // fn set_panics_for_empty() {
    //     let storage = MemoryStore::new();
    //     let mut store = storage.writer();
    //     let gas = GasMeter::infinite();
    //     store.set(&gas, b"foo", b"").unwrap();
    // }

    #[test]
    fn delete() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas = GasMeter::infinite();
        store.set(&gas, b"foo", b"bar").unwrap();
        store.set(&gas, b"food", b"bank").unwrap();
        store.remove(&gas, b"foo").unwrap();

        assert_eq!(store.get(&gas, b"foo").unwrap(), None);
        assert_eq!(store.get(&gas, b"food").unwrap(), Some(b"bank".to_vec()));
    }

    #[test]
    fn iterator() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas = GasMeter::infinite();
        store.set(&gas, b"foo", b"bar").unwrap();

        // ensure we had previously set "foo" = "bar"
        assert_eq!(store.get(&gas, b"foo").unwrap(), Some(b"bar".to_vec()));
        assert_eq!(
            store
                .range(&gas, None, None, Order::Ascending)
                .unwrap()
                .count(),
            1
        );

        // setup - add some data, and delete part of it as well
        store.set(&gas, b"ant", b"hill").unwrap();
        store.set(&gas, b"ze", b"bra").unwrap();

        // noise that should be ignored
        store.set(&gas, b"bye", b"bye").unwrap();
        store.remove(&gas, b"bye").unwrap();

        // unbounded
        {
            let iter = store.range(&gas, None, None, Order::Ascending).unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(
                elements,
                vec![
                    (b"ant".to_vec(), b"hill".to_vec()),
                    (b"foo".to_vec(), b"bar".to_vec()),
                    (b"ze".to_vec(), b"bra".to_vec()),
                ]
            );
        }

        // unbounded (descending)
        {
            let iter = store.range(&gas, None, None, Order::Descending).unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(
                elements,
                vec![
                    (b"ze".to_vec(), b"bra".to_vec()),
                    (b"foo".to_vec(), b"bar".to_vec()),
                    (b"ant".to_vec(), b"hill".to_vec()),
                ]
            );
        }

        // bounded
        {
            let iter = store
                .range(&gas, Some(b"f"), Some(b"n"), Order::Ascending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(elements, vec![(b"foo".to_vec(), b"bar".to_vec())]);
        }

        // bounded (descending)
        {
            let iter = store
                .range(&gas, Some(b"air"), Some(b"loop"), Order::Descending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(
                elements,
                vec![
                    (b"foo".to_vec(), b"bar".to_vec()),
                    (b"ant".to_vec(), b"hill".to_vec()),
                ]
            );
        }

        // bounded empty [a, a)
        {
            let iter = store
                .range(&gas, Some(b"foo"), Some(b"foo"), Order::Ascending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(elements, vec![]);
        }

        // bounded empty [a, a) (descending)
        {
            let iter = store
                .range(&gas, Some(b"foo"), Some(b"foo"), Order::Descending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(elements, vec![]);
        }

        // bounded empty [a, b) with b < a
        {
            let iter = store
                .range(&gas, Some(b"z"), Some(b"a"), Order::Ascending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(elements, vec![]);
        }

        // bounded empty [a, b) with b < a (descending)
        {
            let iter = store
                .range(&gas, Some(b"z"), Some(b"a"), Order::Descending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(elements, vec![]);
        }

        // right unbounded
        {
            let iter = store
                .range(&gas, Some(b"f"), None, Order::Ascending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(
                elements,
                vec![
                    (b"foo".to_vec(), b"bar".to_vec()),
                    (b"ze".to_vec(), b"bra".to_vec()),
                ]
            );
        }

        // right unbounded (descending)
        {
            let iter = store
                .range(&gas, Some(b"f"), None, Order::Descending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(
                elements,
                vec![
                    (b"ze".to_vec(), b"bra".to_vec()),
                    (b"foo".to_vec(), b"bar".to_vec()),
                ]
            );
        }

        // left unbounded
        {
            let iter = store
                .range(&gas, None, Some(b"f"), Order::Ascending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(elements, vec![(b"ant".to_vec(), b"hill".to_vec()),]);
        }

        // left unbounded (descending)
        {
            let iter = store
                .range(&gas, None, Some(b"no"), Order::Descending)
                .unwrap();
            let elements = iter.collect::<Result<Vec<Record>, GasError>>().unwrap();
            assert_eq!(
                elements,
                vec![
                    (b"foo".to_vec(), b"bar".to_vec()),
                    (b"ant".to_vec(), b"hill".to_vec()),
                ]
            );
        }
    }
    /*

        #[test]
        fn memory_storage_implements_debug() {
            let store = BTreeStorage::new();
            assert_eq!(
                format!("{:?}", store),
                "MemoryStorage (0 entries) {\n\
                }"
            );

            // With one element
            let mut store = BTreeStorage::new();
            store.set(&[0x00, 0xAB, 0xDD], &[0xFF, 0xD5]);
            assert_eq!(
                format!("{:?}", store),
                "MemoryStorage (1 entries) {\n\
                \x20\x200x00abdd: 0xffd5\n\
                }"
            );

            // Sorted by key
            let mut store = BTreeStorage::new();
            store.set(&[0x00, 0xAB, 0xDD], &[0xFF, 0xD5]);
            store.set(&[0x00, 0xAB, 0xEE], &[0xFF, 0xD5]);
            store.set(&[0x00, 0xAB, 0xCC], &[0xFF, 0xD5]);
            assert_eq!(
                format!("{:?}", store),
                "MemoryStorage (3 entries) {\n\
                \x20\x200x00abcc: 0xffd5\n\
                \x20\x200x00abdd: 0xffd5\n\
                \x20\x200x00abee: 0xffd5\n\
                }"
            );

            // Different lengths
            let mut store = BTreeStorage::new();
            store.set(&[0xAA], &[0x11]);
            store.set(&[0xAA, 0xBB], &[0x11, 0x22]);
            store.set(&[0xAA, 0xBB, 0xCC], &[0x11, 0x22, 0x33]);
            store.set(&[0xAA, 0xBB, 0xCC, 0xDD], &[0x11, 0x22, 0x33, 0x44]);
            assert_eq!(
                format!("{:?}", store),
                "MemoryStorage (4 entries) {\n\
                \x20\x200xaa: 0x11\n\
                \x20\x200xaabb: 0x1122\n\
                \x20\x200xaabbcc: 0x112233\n\
                \x20\x200xaabbccdd: 0x11223344\n\
                }"
            );
        }
    */
}
