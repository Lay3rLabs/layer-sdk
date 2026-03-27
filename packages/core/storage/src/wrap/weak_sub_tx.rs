use crate::{PriceList, ReadonlyStorage, Storage, DEFAULT_CACHE_PRICES};
use cosmwasm_std::{Order, Record};
use layer_std::{GasMeter, GasResult};
use tracing::trace_span;

use super::{Op, ReaderWrapper};

pub struct WeakSubTx<'a> {
    /// read-only access to backing storage
    storage: &'a dyn ReadonlyStorage,
    /// these are local changes not flushed to backing storage
    wrap: ReaderWrapper,
    price_list: PriceList,
}

impl<'a> WeakSubTx<'a> {
    pub fn new(storage: &'a dyn ReadonlyStorage) -> Self {
        WeakSubTx {
            storage,
            wrap: ReaderWrapper::new(),
            price_list: DEFAULT_CACHE_PRICES,
        }
    }

    /// prepares this transaction to be committed to storage
    pub fn prepare(self) -> RepLog {
        let ops = self.wrap.prepare();
        RepLog { ops }
    }
}

impl ReadonlyStorage for WeakSubTx<'_> {
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let _span = trace_span!("get").entered();
        let value = self.wrap.get(self.storage, meter, key)?;
        self.price_list.charge_read(meter, key, value.as_deref())?;
        Ok(value)
    }

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>> {
        let _span = trace_span!("range").entered();
        self.price_list.charge_range(meter)?;
        self.wrap.range(self.storage, meter, start, end, order)
    }
    fn abort(self) {}
}

impl Storage for WeakSubTx<'_> {
    fn set(&mut self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        let _span = trace_span!("set").entered();
        self.price_list.charge_range(meter)?;
        self.wrap.set(meter, key, value)
    }

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        let _span = trace_span!("remove").entered();
        self.price_list.charge_remove(meter, key)?;
        self.wrap.remove(meter, key)
    }

    fn as_ref(&self) -> &dyn ReadonlyStorage {
        self
    }
}
pub struct RepLog {
    /// this is a list of changes to be written to backing storage upon commit
    ops: Vec<Op>,
}

impl RepLog {
    /// applies the stored list of `Op`s to the provided `Storage`
    pub fn commit(self, storage: &mut dyn Storage, meter: &GasMeter) -> GasResult<()> {
        for op in self.ops {
            op.apply(storage, meter)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{MemoryStore, PersistentStorage};

    #[test]
    fn wrap_storage() {
        let store = MemoryStore::new();
        let reader = store.reader();
        let meter = GasMeter::infinite();
        let mut wrap = WeakSubTx::new(&reader);
        wrap.set(&meter, b"foo", b"bar").unwrap();

        assert_eq!(None, reader.get(&meter, b"foo").unwrap());
        let ops = wrap.prepare();
        // reader.abort();
        let mut writer = store.writer();
        ops.commit(&mut writer, &meter).unwrap();
        assert_eq!(Some(b"bar".to_vec()), writer.get(&meter, b"foo").unwrap());
    }

    /*
    #[test]
    fn wrap_ref_cell() {
        let store = RefCell::new(MemoryStore::new());
        let ops = {
            let refer = store.borrow();
            let mut wrap = WeakSubTx::new(refer.deref());
            wrap.set(b"foo", b"bar");
            assert_eq!(None, store.borrow().get(b"foo"));
            wrap.prepare()
        };
        ops.commit(store.borrow_mut().deref_mut());
        assert_eq!(Some(b"bar".to_vec()), store.borrow().get(b"foo"));
    }

    #[test]
    fn wrap_box_storage() {
        let mut store: Box<MemoryStore> = Box::new(MemoryStore::new());
        let mut wrap = WeakSubTx::new(store.as_ref());
        wrap.set(b"foo", b"bar");

        assert_eq!(None, store.get(b"foo"));
        wrap.prepare().commit(store.as_mut());
        assert_eq!(Some(b"bar".to_vec()), store.get(b"foo"));
    }

    #[test]
    fn wrap_box_dyn_storage() {
        let mut store: Box<dyn Storage> = Box::new(MemoryStore::new());
        let mut wrap = WeakSubTx::new(store.as_ref());
        wrap.set(b"foo", b"bar");

        assert_eq!(None, store.get(b"foo"));
        wrap.prepare().commit(store.as_mut());
        assert_eq!(Some(b"bar".to_vec()), store.get(b"foo"));
    }

    #[test]
    fn wrap_ref_cell_dyn_storage() {
        let inner: Box<dyn Storage> = Box::new(MemoryStore::new());
        let store = RefCell::new(inner);
        // Tricky but working
        // 1. we cannot inline WeakSubTx::new(store.borrow().as_ref()) as Ref must outlive WeakSubTx
        // 2. we cannot call ops.commit() until refer is out of scope - borrow_mut() and borrow() on the same object
        // This can work with some careful scoping, this provides a good reference
        let ops = {
            let refer = store.borrow();
            let mut wrap = WeakSubTx::new(refer.as_ref());
            wrap.set(b"foo", b"bar");

            assert_eq!(None, store.borrow().get(b"foo"));
            wrap.prepare()
        };
        ops.commit(store.borrow_mut().as_mut());
        assert_eq!(Some(b"bar".to_vec()), store.borrow().get(b"foo"));
    }

    #[cfg(feature = "iterator")]
    // iterator_test_suite takes a storage, adds data and runs iterator tests
    // the storage must previously have exactly one key: "foo" = "bar"
    // (this allows us to test WeakSubTx and other wrapped storage better)
    fn iterator_test_suite<S: Storage>(store: &mut S) {
        // ensure we had previously set "foo" = "bar"
        assert_eq!(store.get(b"foo"), Some(b"bar".to_vec()));
        assert_eq!(store.range(None, None, Order::Ascending).count(), 1);

        // setup - add some data, and delete part of it as well
        store.set(b"ant", b"hill");
        store.set(b"ze", b"bra");

        // noise that should be ignored
        store.set(b"bye", b"bye");
        store.remove(b"bye");

        // unbounded
        {
            let iter = store.range(None, None, Order::Ascending);
            let elements: Vec<Record> = iter.collect();
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
            let iter = store.range(None, None, Order::Descending);
            let elements: Vec<Record> = iter.collect();
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
            let iter = store.range(Some(b"f"), Some(b"n"), Order::Ascending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(elements, vec![(b"foo".to_vec(), b"bar".to_vec())]);
        }

        // bounded (descending)
        {
            let iter = store.range(Some(b"air"), Some(b"loop"), Order::Descending);
            let elements: Vec<Record> = iter.collect();
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
            let iter = store.range(Some(b"foo"), Some(b"foo"), Order::Ascending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(elements, vec![]);
        }

        // bounded empty [a, a) (descending)
        {
            let iter = store.range(Some(b"foo"), Some(b"foo"), Order::Descending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(elements, vec![]);
        }

        // bounded empty [a, b) with b < a
        {
            let iter = store.range(Some(b"z"), Some(b"a"), Order::Ascending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(elements, vec![]);
        }

        // bounded empty [a, b) with b < a (descending)
        {
            let iter = store.range(Some(b"z"), Some(b"a"), Order::Descending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(elements, vec![]);
        }

        // right unbounded
        {
            let iter = store.range(Some(b"f"), None, Order::Ascending);
            let elements: Vec<Record> = iter.collect();
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
            let iter = store.range(Some(b"f"), None, Order::Descending);
            let elements: Vec<Record> = iter.collect();
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
            let iter = store.range(None, Some(b"f"), Order::Ascending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(elements, vec![(b"ant".to_vec(), b"hill".to_vec()),]);
        }

        // left unbounded (descending)
        {
            let iter = store.range(None, Some(b"no"), Order::Descending);
            let elements: Vec<Record> = iter.collect();
            assert_eq!(
                elements,
                vec![
                    (b"foo".to_vec(), b"bar".to_vec()),
                    (b"ant".to_vec(), b"hill".to_vec()),
                ]
            );
        }
    }

    #[test]
    fn delete_local() {
        let mut base = Box::new(MemoryStore::new());
        let mut check = WeakSubTx::new(base.as_ref());
        check.set(b"foo", b"bar");
        check.set(b"food", b"bank");
        check.remove(b"foo");

        assert_eq!(check.get(b"foo"), None);
        assert_eq!(check.get(b"food"), Some(b"bank".to_vec()));

        // now commit to base and query there
        check.prepare().commit(base.as_mut());
        assert_eq!(base.get(b"foo"), None);
        assert_eq!(base.get(b"food"), Some(b"bank".to_vec()));
    }

    #[test]
    fn delete_from_base() {
        let mut base = Box::new(MemoryStore::new());
        base.set(b"foo", b"bar");
        let mut check = WeakSubTx::new(base.as_ref());
        check.set(b"food", b"bank");
        check.remove(b"foo");

        assert_eq!(check.get(b"foo"), None);
        assert_eq!(check.get(b"food"), Some(b"bank".to_vec()));

        // now commit to base and query there
        check.prepare().commit(base.as_mut());
        assert_eq!(base.get(b"foo"), None);
        assert_eq!(base.get(b"food"), Some(b"bank".to_vec()));
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn storage_transaction_iterator_empty_base() {
        let base = MemoryStore::new();
        let mut check = WeakSubTx::new(&base);
        check.set(b"foo", b"bar");
        iterator_test_suite(&mut check);
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn storage_transaction_iterator_with_base_data() {
        let mut base = MemoryStore::new();
        base.set(b"foo", b"bar");
        let mut check = WeakSubTx::new(&base);
        iterator_test_suite(&mut check);
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn storage_transaction_iterator_removed_items_from_base() {
        let mut base = Box::new(MemoryStore::new());
        base.set(b"foo", b"bar");
        base.set(b"food", b"bank");
        let mut check = WeakSubTx::new(base.as_ref());
        check.remove(b"food");
        iterator_test_suite(&mut check);
    }

    #[test]
    fn commit_writes_through() {
        let mut base = Box::new(MemoryStore::new());
        base.set(b"foo", b"bar");

        let mut check = WeakSubTx::new(base.as_ref());
        assert_eq!(check.get(b"foo"), Some(b"bar".to_vec()));
        check.set(b"subtx", b"works");
        check.prepare().commit(base.as_mut());

        assert_eq!(base.get(b"subtx"), Some(b"works".to_vec()));
    }

    #[test]
    fn storage_remains_readable() {
        let mut base = MemoryStore::new();
        base.set(b"foo", b"bar");

        let mut stxn1 = WeakSubTx::new(&base);

        assert_eq!(stxn1.get(b"foo"), Some(b"bar".to_vec()));

        stxn1.set(b"subtx", b"works");
        assert_eq!(stxn1.get(b"subtx"), Some(b"works".to_vec()));

        // Can still read from base, txn is not yet committed
        assert_eq!(base.get(b"subtx"), None);

        stxn1.prepare().commit(&mut base);
        assert_eq!(base.get(b"subtx"), Some(b"works".to_vec()));
    }

    #[test]
    fn ignore_same_as_rollback() {
        let mut base = MemoryStore::new();
        base.set(b"foo", b"bar");

        let mut check = WeakSubTx::new(&base);
        assert_eq!(check.get(b"foo"), Some(b"bar".to_vec()));
        check.set(b"subtx", b"works");

        assert_eq!(base.get(b"subtx"), None);
    }
    */
}
