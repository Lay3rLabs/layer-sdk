use std::pin::Pin;

use futures::Stream;

use cosmwasm_std::{Order, Record};
use layer_std::{stringify_or_hex, GasMeter, GasResult};

/// This is the lowest level of the storage, which can be implemented by MemoryStorage
/// or a real on-disk database. It provides ReadAccessors like MeteredStorage,
/// but one method for bulk write, that will commit a new version and return the app hash (stored internally)
pub trait PersistentStorage: SyncableStorage {
    type Reader<'x>: ReadonlyStorage
    where
        Self: 'x;

    type Writer<'x>: Transaction
    where
        Self: 'x;

    // open a read-only view of the storage. should abort it to free space for write
    fn reader(&self) -> Self::Reader<'_>;

    // open a read-write view of the storage. takes exclusive access to the storage until completed
    // assumes internal rwlock
    fn writer(&self) -> Self::Writer<'_>;

    /// Returns app hash of last commit
    fn app_hash(&self) -> Vec<u8>;
}

pub type KV = (Vec<u8>, Vec<u8>);

pub trait SyncableStorage {
    fn latest_sequence(&self) -> u64;
    fn current_state(&self) -> Pin<Box<dyn Stream<Item = Result<KV, String>> + Send>>;
    fn changes_since(
        &self,
        _sequence: u64,
    ) -> Pin<Box<dyn Stream<Item = Result<BatchChanges, String>> + Send>>;
}

#[derive(Debug, PartialEq)]
pub struct BatchChanges {
    pub sequence: u64,
    pub changes: Vec<StateUpdate>,
}

#[derive(PartialEq)]
pub enum StateUpdate {
    Write { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

impl std::fmt::Debug for StateUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write { key, value } => f
                .debug_struct("Write")
                .field("key", &stringify_or_hex(key))
                .field("value", &stringify_or_hex(value))
                .finish(),
            Self::Delete { key } => f
                .debug_struct("Delete")
                .field("key", &stringify_or_hex(key))
                .finish(),
        }
    }
}

/// This is like cosmwasm_std::Storage, but takes GasMeter as extra arg everywhere
pub trait ReadonlyStorage {
    /// Drops this reader or transaction without committing changes
    /// May be needed to free up resources
    fn abort(self);

    fn get(&self, meter: &GasMeter, key: &[u8]) -> GasResult<Option<Vec<u8>>>;

    fn range<'a>(
        &'a self,
        meter: &'a GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'a>>;
}

pub trait Storage: ReadonlyStorage {
    fn set(&mut self, meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()>;

    fn remove(&mut self, meter: &GasMeter, key: &[u8]) -> GasResult<()>;

    fn as_ref(&self) -> &dyn ReadonlyStorage;
}

pub trait Transaction: Storage {
    // This writes all changes to the underlying storage and consumes this wrapper
    fn commit(self, meter: &GasMeter) -> GasResult<()>;

    fn as_mut(&mut self) -> &mut dyn Storage;
}
