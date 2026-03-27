use tracing::trace_span;

use cosmwasm_std::{Order, Record};
use layer_std::{GasMeter, GasResult};

use super::ReaderWrapper;
use crate::{traits::Transaction, PriceList, ReadonlyStorage, Storage, DEFAULT_CACHE_PRICES};

/// This wraps ReadonlyStorage, providing a scratch-pad that acts like normal Storage
/// but can never be persisted to the underlying state.
///
/// Useful for things like CheckTx and SimulateQuery that may execute code that normally
/// updates state, but do so as part of a read-only transaction.
pub struct ScratchTx<'a> {
    storage: &'a dyn ReadonlyStorage,
    wrap: ReaderWrapper,
    price_list: PriceList,
}

impl<'a> ScratchTx<'a> {
    pub fn new(storage: &'a dyn ReadonlyStorage) -> Self {
        ScratchTx {
            storage,
            wrap: ReaderWrapper::new(),
            price_list: DEFAULT_CACHE_PRICES,
        }
    }
}

impl ReadonlyStorage for ScratchTx<'_> {
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

impl Storage for ScratchTx<'_> {
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

impl Transaction for ScratchTx<'_> {
    // FIXME: better error message - this should never be called, but we expose the API for the trait.
    // Shall we make it no op rather than panic??
    fn commit(self, _meter: &GasMeter) -> GasResult<()> {
        unimplemented!()
    }

    fn as_mut(&mut self) -> &mut dyn Storage {
        self
    }
}
