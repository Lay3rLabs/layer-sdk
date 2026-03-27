use tracing::trace_span;

use cosmwasm_std::{Order, Record};
use layer_std::{GasError, GasMeter, GasResult};

use super::{Delta, ReaderWrapper};
use crate::{
    traits::Transaction, PriceList, ReadonlyStorage, ScratchTx, Storage, DEFAULT_CACHE_PRICES,
};

pub fn atomic<T, E: From<GasError>>(
    storage: &mut dyn Storage,
    meter: &GasMeter,
    f: impl FnOnce(&mut SubTx, &GasMeter) -> Result<T, E>,
) -> Result<T, E> {
    let mut tx = SubTx::new(storage);
    let res = f(&mut tx, meter);
    if res.is_ok() {
        tx.commit(meter)?;
    } else {
        tx.abort();
    }
    res
}

/// This can wrap any Storage with another one as temporary transaction level on top.
/// If aborted, all writes will be discarded.
/// If committed, all writes will be written to the underlying storage.
/// Holds an exclusive lock (&mut) on the Storage until committed or aborted
pub struct SubTx<'a> {
    storage: &'a mut dyn Storage,
    wrap: ReaderWrapper,
    price_list: PriceList,
}

impl<'a> SubTx<'a> {
    pub fn new(storage: &'a mut dyn Storage) -> Self {
        SubTx {
            storage,
            wrap: ReaderWrapper::new(),
            price_list: DEFAULT_CACHE_PRICES,
        }
    }

    pub fn scratch<'b>(&'b self) -> Box<dyn Storage + 'b> {
        Box::new(ScratchTx::new(self))
    }
}

impl ReadonlyStorage for SubTx<'_> {
    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        let _span = trace_span!("get").entered();
        let value = self.wrap.get(self.storage.as_ref(), meter, key)?;
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
        self.wrap
            .range(self.storage.as_ref(), meter, start, end, order)
    }
    fn abort(self) {}
}

impl Storage for SubTx<'_> {
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

impl Transaction for SubTx<'_> {
    // FIXME: better error message - this should never be called, but we expose the API for the trait.
    // Shall we make it no op rather than panic??
    fn commit(self, meter: &GasMeter) -> GasResult<()> {
        let _span = trace_span!("commit").entered();
        // Write directly to underlying storage without intermediate vector
        for (key, delta) in self.wrap.local_state.into_iter() {
            match delta {
                Delta::Set { value } => {
                    self.storage.set(meter, &key, &value)?;
                }
                Delta::Delete {} => {
                    self.storage.remove(meter, &key)?;
                }
            }
        }
        Ok(())
    }

    fn as_mut(&mut self) -> &mut dyn Storage {
        self
    }
}
