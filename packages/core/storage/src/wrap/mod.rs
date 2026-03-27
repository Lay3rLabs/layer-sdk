mod reader_wrap;
mod scratch_tx;
mod sub_tx;
mod weak_sub_tx;

use layer_std::{GasMeter, GasResult};
pub(crate) use reader_wrap::ReaderWrapper;
pub use scratch_tx::ScratchTx;
pub use sub_tx::{atomic, SubTx};
pub use weak_sub_tx::{RepLog, WeakSubTx};

use crate::Storage;

/// The BTreeMap specific key-value pair reference type, as returned by BTreeMap<Vec<u8>, T>::range.
/// This is internal as it can change any time if the map implementation is swapped out.
type BTreeMapPairRef<'a, T = Vec<u8>> = (&'a Vec<u8>, &'a T);

/// Op is the user operation, which can be stored in the RepLog.
/// Currently Set or Delete.
pub(crate) enum Op {
    /// represents the `Set` operation for setting a key-value pair in storage
    Set {
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        key: Vec<u8>,
    },
}

impl Op {
    pub fn from_delta((key, delta): (Vec<u8>, Delta)) -> Self {
        match delta {
            Delta::Set { value } => Op::Set { key, value },
            Delta::Delete {} => Op::Delete { key },
        }
    }

    /// applies this `Op` to the provided storage
    pub fn apply(&self, storage: &mut dyn Storage, meter: &GasMeter) -> GasResult<()> {
        match self {
            Op::Set { key, value } => storage.set(meter, key, value),
            Op::Delete { key } => storage.remove(meter, key),
        }
    }
}

/// Delta is the changes, stored in the local transaction cache.
/// This is either Set{value} or Delete{}. Note that this is the "value"
/// part of a BTree, so the Key (from the Op) is stored separately.
pub(crate) enum Delta {
    Set { value: Vec<u8> },
    Delete {},
}
