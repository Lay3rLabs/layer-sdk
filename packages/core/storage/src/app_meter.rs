use layer_std::{GasMeter, GasResult};

use crate::{PriceList, ReadonlyStorage, Storage, DEFAULT_COMMIT_PRICES};

/// This is a simple wrapper around a storage that keeps track of the gas used.
pub struct AppMeter<'a> {
    storage: &'a mut dyn Storage,
    price_list: PriceList,
}

impl<'a> AppMeter<'a> {
    pub fn new(storage: &'a mut dyn Storage) -> Self {
        AppMeter {
            storage,
            price_list: DEFAULT_COMMIT_PRICES,
        }
    }
}

// Pass through readonly calls with no charges
impl ReadonlyStorage for AppMeter<'_> {
    fn abort(self) {}

    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>> {
        self.storage.get(meter, key)
    }

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: cosmwasm_std::Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<cosmwasm_std::Record>> + 'a>> {
        self.storage.range(meter, start, end, order)
    }
}

/// Charge for writes and removes
impl Storage for AppMeter<'_> {
    fn set(&mut self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        self.price_list.charge_write(meter, key, value)?;
        self.storage.set(meter, key, value)
    }

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        self.price_list.charge_remove(meter, key)?;
        self.storage.remove(meter, key)
    }

    fn as_ref(&self) -> &dyn ReadonlyStorage {
        self
    }
}
