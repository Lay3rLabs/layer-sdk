use futures::Stream;
use rocksdb::{DBAccess, OptimisticTransactionDB, Options, WriteBatchIterator};
use std::marker::PhantomData;
use std::ops::DerefMut;
use std::path::Path;
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::{fmt, thread};
use tokio::sync::mpsc::Receiver;

use cosmwasm_std::{Order, Record};
use layer_std::{GasMeter, GasResult};

use crate::traits::KV;
use crate::{
    traits::{BatchChanges, StateUpdate},
    FastHasher, PersistentStorage, PriceList, ReadonlyStorage, Storage, SyncableStorage,
    Transaction, DEFAULT_PERSISTED_PRICES,
};

pub struct RockStore {
    pub db: Arc<OptimisticTransactionDB>,
    /// How many ms to wait before trying to check for new wal changeset
    pub changes_retry_ms: u64,
}

const DEFAULT_CHANGES_RETRY_MS: u64 = 200;

// TODO: tune dynamically
const NUM_CPUS: i32 = 8;

// Underscore means it is not appended to the hasher,
// Everything should be namespaced and thus avoid collision
pub const APP_HASH_KEY: &[u8] = b"_app_hash";

impl RockStore {
    /// opens or creates a store
    pub fn open<P: AsRef<Path>>(path: P) -> RockStore {
        let db_opts = RockStore::default_db_opts();
        RockStore::open_opts(path, db_opts)
    }

    pub fn open_opts<P: AsRef<Path>>(path: P, opts: Options) -> RockStore {
        let db = OptimisticTransactionDB::open(&opts, path).unwrap();
        // TODO: make this configurable
        RockStore {
            db: Arc::new(db),
            changes_retry_ms: DEFAULT_CHANGES_RETRY_MS,
        }
    }

    fn default_db_opts() -> Options {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_atomic_flush(true);

        opts.increase_parallelism(NUM_CPUS);
        opts.set_allow_mmap_writes(true);
        opts.set_allow_mmap_reads(true);

        opts.set_max_log_file_size(1_000_000);
        opts.set_recycle_log_file_num(5);
        opts.set_keep_log_file_num(5);
        opts.set_log_level(rocksdb::LogLevel::Warn);

        opts
    }
}

impl fmt::Debug for RockStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // FIXME: add some path info?
        write!(f, "RockStore")
    }
}

impl PersistentStorage for RockStore {
    type Reader<'a> = RockReader<'a>;

    type Writer<'a> = RockWriter<'a>;

    // open a read-only view of the storage. should abort it to free space for write
    fn reader(&self) -> RockReader<'_> {
        RockReader {
            db: self.db.clone(),
            price_list: DEFAULT_PERSISTED_PRICES,
            lifetime: PhantomData,
        }
    }

    // open a read-write view of the storage. takes exclusive access to the storage until completed
    // assumes internal rwlock
    fn writer(&self) -> RockWriter<'_> {
        RockWriter {
            transaction: self.db.transaction(),
            price_list: DEFAULT_PERSISTED_PRICES,
            hasher: FastHasher::new(&self.app_hash()),
        }
    }

    /// Returns app hash of last commit
    fn app_hash(&self) -> Vec<u8> {
        // load the key or return the empty first block hash (all 0)
        self.db
            .get(APP_HASH_KEY)
            .unwrap()
            .unwrap_or_else(|| vec![0; 32])
    }
}

impl SyncableStorage for RockStore {
    fn latest_sequence(&self) -> u64 {
        self.db.latest_sequence_number()
    }

    // This uses a thread in order to hold no references to the DB.
    fn current_state(&self) -> Pin<Box<dyn Stream<Item = Result<KV, String>> + Send>> {
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let db = self.db.clone();
        thread::spawn(move || {
            let items = db.iterator(rocksdb::IteratorMode::Start);
            for item in items {
                let (k, v) = match item {
                    Ok(x) => x,
                    Err(e) => {
                        let _ = sender.blocking_send(Err(e.to_string()));
                        return;
                    }
                };
                let out = (k.into_vec(), v.into_vec());
                // don't error if channel dropped, just exit early
                if sender.blocking_send(Ok(out)).is_err() {
                    return;
                }
            }
        });
        Box::pin(ReceiverStream(receiver))
    }

    fn changes_since(
        &self,
        sequence: u64,
    ) -> Pin<Box<dyn Stream<Item = Result<BatchChanges, String>> + Send>> {
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let db = self.db.clone();
        let retry_ms = self.changes_retry_ms;

        // This should go forever, until the channel is dropped
        thread::spawn(move || {
            let mut start_from = sequence;
            loop {
                let changes = match db.get_updates_since(start_from) {
                    Ok(c) => c,
                    Err(e) => {
                        // explicitly ignore errors here
                        let _ = sender.blocking_send(Err(e.to_string()));
                        return;
                    }
                };
                for item in changes {
                    match item {
                        Ok((sequence, batch)) => {
                            let mut capture = CaptureBatch::new(batch.len());
                            batch.iterate(&mut capture);
                            let val = BatchChanges {
                                sequence,
                                changes: capture.changes,
                            };
                            // don't error if channel dropped, just exit early
                            if sender.blocking_send(Ok(val)).is_err() {
                                return;
                            }
                            start_from = sequence + 1;
                        }
                        Err(e) => {
                            // explicitly ignore errors here
                            let _ = sender.blocking_send(Err(e.to_string()));
                            return;
                        }
                    }
                }
                // add a small pause here to avoid crazy load. once we hit the end of changelog, we wait 300ms to check again
                thread::sleep(std::time::Duration::from_millis(retry_ms));
            }
        });
        Box::pin(ReceiverStream(receiver))
    }
}
pub struct ReceiverStream<T>(Receiver<Result<T, String>>);

impl<T> Stream for ReceiverStream<T> {
    type Item = Result<T, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let fut = self.deref_mut().0.recv();
        let fut = pin!(fut);
        std::future::Future::poll(fut, cx)
    }
}

struct CaptureBatch {
    changes: Vec<StateUpdate>,
}

impl CaptureBatch {
    pub fn new(len: usize) -> Self {
        CaptureBatch {
            changes: Vec::with_capacity(len),
        }
    }
}

impl WriteBatchIterator for CaptureBatch {
    fn put(&mut self, key: Box<[u8]>, value: Box<[u8]>) {
        let change = StateUpdate::Write {
            key: key.into_vec(),
            value: value.into_vec(),
        };
        self.changes.push(change);
    }

    fn delete(&mut self, key: Box<[u8]>) {
        let change = StateUpdate::Delete {
            key: key.into_vec(),
        };
        self.changes.push(change);
    }
}

pub struct RockReader<'a> {
    db: Arc<OptimisticTransactionDB>,
    price_list: PriceList,
    lifetime: PhantomData<&'a ()>,
}

impl<'a> ReadonlyStorage for RockReader<'a> {
    fn abort(self) {}

    // NOTE: if we parse it into an object here, we would only need &[u8]
    // and could use get_pinned, avoiding a copy
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let val = self.db.get(key).unwrap();
        self.price_list.charge_read(meter, key, val.as_deref())?;
        Ok(val)
    }

    fn range<'b>(
        &'b self,
        meter: &'b GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'b>> {
        self.price_list.charge_range(meter)?;
        let iter = self.db.raw_iterator();
        let res = RockIterator::new(iter, order, start, end, meter, self.price_list);
        Ok(Box::new(res))
    }
}

pub struct RockWriter<'a> {
    transaction: rocksdb::Transaction<'a, OptimisticTransactionDB>,
    price_list: PriceList,
    hasher: FastHasher,
}

impl<'a> ReadonlyStorage for RockWriter<'a> {
    fn abort(self) {}

    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let val = self.transaction.get(key).unwrap();
        self.price_list.charge_read(meter, key, val.as_deref())?;
        Ok(val)
    }

    fn range<'b>(
        &'b self,
        meter: &'b GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'b>> {
        self.price_list.charge_range(meter)?;
        let iter = self.transaction.raw_iterator();
        let res = RockIterator::new(iter, order, start, end, meter, self.price_list);
        Ok(Box::new(res))
    }
}

impl<'a> Storage for RockWriter<'a> {
    fn set(&mut self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        // TODO: use transaction or manual batching...
        self.price_list.charge_write(meter, key, value)?;
        self.transaction.put(key, value).unwrap();
        self.hasher.set(key, value);
        Ok(())
    }

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        self.price_list.charge_remove(meter, key)?;
        self.transaction.delete(key).unwrap();
        self.hasher.remove(key);
        Ok(())
    }

    fn as_ref(&self) -> &dyn ReadonlyStorage {
        self
    }
}

impl<'a> Transaction for RockWriter<'a> {
    // This writes all changes to the underlying storage and consumes this wrapper
    fn commit(self, _meter: &GasMeter) -> GasResult<()> {
        // calculate and update app_hash
        let app_hash = self.hasher.hash();
        self.transaction.put(APP_HASH_KEY, app_hash).unwrap();
        self.transaction.commit().unwrap();
        Ok(())
    }

    fn as_mut(&mut self) -> &mut dyn Storage {
        self
    }
}

pub struct RockIterator<'a, T: DBAccess> {
    iterator: rocksdb::DBRawIteratorWithThreadMode<'a, T>,
    order: Order,
    stop: Option<Vec<u8>>,
    meter: &'a GasMeter,
    price_list: PriceList,
    finished: bool,
}

impl<'a, T: DBAccess> RockIterator<'a, T> {
    pub fn new(
        mut iter: rocksdb::DBRawIteratorWithThreadMode<'a, T>,
        order: Order,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        meter: &'a GasMeter,
        price_list: PriceList,
    ) -> Self {
        let stop = match order {
            Order::Ascending => {
                match start {
                    Some(begin) => iter.seek(begin),
                    None => iter.seek_to_first(),
                }
                end
            }
            Order::Descending => {
                match end {
                    // Note: our semantics specify we do not include end, yet seek_for_prev will
                    // stop on an exact match. If it is a perfect match, we have to go one before
                    Some(finish) => {
                        iter.seek_for_prev(finish);
                        if let Some(k) = iter.key() {
                            if k == finish {
                                iter.prev()
                            }
                        }
                    }
                    None => iter.seek_to_last(),
                }
                start
            }
        }
        .map(Vec::from);

        Self {
            iterator: iter,
            order,
            stop,
            meter,
            price_list,
            finished: false,
        }
    }
}

impl<'a, T: DBAccess> Iterator for RockIterator<'a, T> {
    type Item = GasResult<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        // Note, when it is created, it key, value already have the
        // first value we want to return. We should only advance with
        // next / prev after processing the current location and preparing
        // the return value
        if self.finished {
            return None;
        }

        let record: Option<Record> = match self.order {
            Order::Ascending => {
                // get next one, and ensure it is not after end
                let record = match (self.iterator.key(), self.stop.as_deref()) {
                    (None, _) => None,
                    // We don't include the higher stop value
                    (Some(k), Some(e)) if k >= e => None,
                    (Some(k), _) => Some((k.to_owned(), self.iterator.value().unwrap().to_owned())),
                };
                self.iterator.next();
                record
            }
            Order::Descending => {
                // get previous one, and ensure it is not before end
                let record = match (self.iterator.key(), self.stop.as_deref()) {
                    (None, _) => None,
                    // we do include the lower stop value
                    (Some(k), Some(e)) if k < e => None,
                    (Some(k), _) => Some((k.to_owned(), self.iterator.value().unwrap().to_owned())),
                };
                self.iterator.prev();
                record
            }
        };

        match record {
            Some(r) => {
                let res = self
                    .price_list
                    .charge_read(self.meter, &r.0, Some(&r.1))
                    .and(Ok(r));
                Some(res)
            }
            None => {
                self.finished = true;
                None
            }
        }
    }
}

#[cfg(test)]
mod rock_tests {
    use super::*;

    use layer_std::GasError;
    use std::path::PathBuf;

    // prepares the path before calling the function
    fn prep(name: &str) -> PathBuf {
        let path = PathBuf::from_iter(&["/tmp/slay3r-rocks", name]);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn get_and_set() {
        let storage = RockStore::open(prep("foo1"));
        let mut store = storage.writer();
        let gas = GasMeter::infinite();
        assert_eq!(store.get(&gas, b"foo").unwrap(), None);
        store.set(&gas, b"foo", b"bar").unwrap();
        assert_eq!(store.get(&gas, b"foo").unwrap(), Some(b"bar".to_vec()));
        assert_eq!(store.get(&gas, b"food").unwrap(), None);
    }

    #[test]
    fn read_on_commit() {
        let storage = RockStore::open(prep("foo2"));
        let gas = GasMeter::infinite();

        // start a tx
        let mut store = storage.writer();
        assert_eq!(store.get(&gas, b"foo").unwrap(), None);
        store.set(&gas, b"foo", b"bar").unwrap();
        assert_eq!(store.get(&gas, b"foo").unwrap(), Some(b"bar".to_vec()));

        // uncommitted tx won't show up in reader query
        let reader = storage.reader();
        assert_eq!(reader.get(&gas, b"foo").unwrap(), None);

        // now commit and it shows up in the reader
        store.commit(&gas).unwrap();
        assert_eq!(reader.get(&gas, b"foo").unwrap(), Some(b"bar".to_vec()));
        assert_eq!(reader.get(&gas, b"food").unwrap(), None);
    }

    #[test]
    fn delete() {
        let storage = RockStore::open(prep("foo3"));
        let mut store = storage.writer();
        let gas = GasMeter::infinite();
        store.set(&gas, b"foo", b"bar").unwrap();
        store.set(&gas, b"food", b"bank").unwrap();
        store.remove(&gas, b"foo").unwrap();

        assert_eq!(store.get(&gas, b"foo").unwrap(), None);
        assert_eq!(store.get(&gas, b"food").unwrap(), Some(b"bank".to_vec()));
    }

    #[test]
    fn app_hash_updates() {
        let storage = RockStore::open(prep("foo5"));
        let gas = GasMeter::infinite();
        let orig_hash = storage.app_hash();

        // write something, new hash
        let mut store = storage.writer();
        store.set(&gas, b"foo", b"bar").unwrap();
        store.set(&gas, b"angel", b"food").unwrap();
        store.commit(&gas).unwrap();
        let hash2 = storage.app_hash();
        assert_ne!(hash2, orig_hash);

        // delete something, new hash
        let mut store = storage.writer();
        store.remove(&gas, b"foo").unwrap();
        store.commit(&gas).unwrap();
        let hash3 = storage.app_hash();
        assert_ne!(hash3, hash2);

        // do nothing, hash remains the same
        let store = storage.writer();
        store.commit(&gas).unwrap();
        let hash4 = storage.app_hash();
        assert_eq!(hash4, hash3);
    }

    #[test]
    fn iterator() {
        let storage = RockStore::open(prep("foo4"));
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
}
