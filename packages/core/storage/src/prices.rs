use layer_std::{GasMeter, GasResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriceList {
    // gas cost for a read
    pub read_flat: u64,
    // gas cost per byte read (* 100)
    pub read_per_byte_percent: u64,

    // gas cost for a write
    pub write_flat: u64,
    // gas cost per byte write (* 100)
    pub write_per_byte_percent: u64,

    // gas cost for a remove
    pub remove_flat: u64,
    // gas cost per byte remove (* 100)
    pub remove_per_byte_percent: u64,

    // gas cost for a range
    pub range_flat: u64,
}

/// We charge for the reads from persisted store. Writes happen at end of block, not in a tx.
/// We use DEFAULT_COMMIT_PRICES to charge for writes.
pub const DEFAULT_PERSISTED_PRICES: PriceList = PriceList {
    read_flat: 1000,
    read_per_byte_percent: 100,
    write_flat: 0,
    write_per_byte_percent: 0,
    remove_flat: 0,
    remove_per_byte_percent: 0,
    range_flat: 1000,
};

/// This should be set on the last level of cache, when performing the actual app-level logic.
/// Expecially with wasmd, sub msg will have different cache levels, and we need to charge carefully to avoid double-charging.
pub const DEFAULT_COMMIT_PRICES: PriceList = PriceList {
    // we need to charge something for reads, so we aren't so slow in a read loop
    read_flat: 100,
    read_per_byte_percent: 10,
    write_flat: 2000,
    write_per_byte_percent: 200,
    remove_flat: 2000,
    remove_per_byte_percent: 200,
    range_flat: 100,
};

/// For now we charge nothing for each cache level, but we could add some minor cost to prevent abuse of memory storage.
/// Note this applies to every level, and tx may be many levels deep.
pub const DEFAULT_CACHE_PRICES: PriceList = PriceList {
    read_flat: 0,
    read_per_byte_percent: 0,
    write_flat: 0,
    write_per_byte_percent: 0,
    remove_flat: 0,
    remove_per_byte_percent: 0,
    range_flat: 0,
};

impl PriceList {
    pub fn charge_read(&self, meter: &GasMeter, key: &[u8], value: Option<&[u8]>) -> GasResult<()> {
        let val_len = value.map(|x| x.len()).unwrap_or_default();
        let cost = self.read_flat + (key.len() + val_len) as u64 * self.read_per_byte_percent / 100;
        meter.charge(cost)
    }

    pub fn charge_write(&self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        let cost =
            self.write_flat + (key.len() + value.len()) as u64 * self.write_per_byte_percent / 100;
        meter.charge(cost)
    }

    pub fn charge_remove(&self, meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        let cost = self.remove_flat + key.len() as u64 * self.remove_per_byte_percent / 100;
        meter.charge(cost)
    }

    pub fn charge_range(&self, meter: &GasMeter) -> GasResult<()> {
        let cost = self.range_flat;
        meter.charge(cost)
    }
}
