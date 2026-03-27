// Helper code around state sync

use std::pin::Pin;

use futures::{Stream, StreamExt};

use layer_proto::layer::sync::v1::{
    state_change::Event, BlockWrites, DeleteData, StateChange, WriteData,
};
use layer_std::string_account_or_hex;
use layer_storage::{PersistentStorage, StateUpdate};

use crate::app::App;
use crate::{app, auth, bank, wasm};

pub trait SyncProvider {
    fn latest_sequence(&self) -> u64;

    fn current_state(&self) -> Pin<Box<dyn Stream<Item = Result<WriteData, String>> + Send>>;

    fn changes_since(
        &self,
        sequence: u64,
    ) -> Pin<Box<dyn Stream<Item = Result<BlockWrites, String>> + Send>>;
}

// Expose lower-level state sync methods by wrapping the persistent storage
impl<T: PersistentStorage + 'static> SyncProvider for App<T> {
    fn latest_sequence(&self) -> u64 {
        self.storage.latest_sequence()
    }

    fn current_state(&self) -> Pin<Box<dyn Stream<Item = Result<WriteData, String>> + Send>> {
        let it = self.storage.current_state();
        let it = it.map(|r| {
            r.map(|(k, value)| {
                let parsed = parse_key(k);
                WriteData {
                    module: parsed.module,
                    bucket: parsed.bucket,
                    keys: parsed.keys,
                    value,
                }
            })
        });
        Box::pin(it)
    }

    fn changes_since(
        &self,
        sequence: u64,
    ) -> Pin<Box<dyn Stream<Item = Result<BlockWrites, String>> + Send>> {
        let it = self.storage.changes_since(sequence);
        let it = it.map(|batch| {
            batch.map(|b| {
                let events = b
                    .changes
                    .into_iter()
                    .map(|x| {
                        let event = match x {
                            StateUpdate::Write { key, value } => {
                                let parsed = parse_key(key);
                                let data = WriteData {
                                    module: parsed.module,
                                    bucket: parsed.bucket,
                                    keys: parsed.keys,
                                    value,
                                };
                                Event::WriteState(data)
                            }
                            StateUpdate::Delete { key } => {
                                let parsed = parse_key(key);
                                let data = DeleteData {
                                    module: parsed.module,
                                    bucket: parsed.bucket,
                                    keys: parsed.keys,
                                };
                                Event::DeleteState(data)
                            }
                        };
                        StateChange { event: Some(event) }
                    })
                    .collect();
                let sequence = b.sequence;
                BlockWrites { sequence, events }
            })
        });
        Box::pin(it)
    }
}

/****** Helper functions ********/

// Convert from PersistentStorage to the GRPC types

#[derive(Debug)]
pub struct ParsedKey {
    pub module: String,
    pub bucket: String,
    pub keys: Vec<String>,
}

pub fn parse_key(key: Vec<u8>) -> ParsedKey {
    let (module, key) = split_module(key);
    let (bucket, key) = split_bucket(key);

    let keys = match module.as_bytes() {
        // internal use, currently only _last_block Item
        b"" => vec![],
        // TODO: make this explicit, but only two Items for now, so no key
        app::NAMESPACE_APP => vec![],

        // real ones
        auth::NAMESPACE_AUTH => auth::parse_keys(&bucket, key),
        bank::NAMESPACE_BANK => bank::parse_keys(&bucket, key),
        wasm::NAMESPACE_WASM => wasm::parse_keys(&bucket, key),
        _ => unimplemented!(),
    };
    ParsedKey {
        module,
        bucket,
        keys,
    }
}

// This tries to read the cw-storage-plus 2 byte length.
// It returns the beginning of the next item if valid, otherwise None
pub fn cut_point(key: &[u8]) -> Option<usize> {
    // first two bytes are module length
    let len = u16::from_be_bytes(key[0..2].try_into().ok()?);
    let end = (2 + len) as usize;
    if end > key.len() {
        None
    } else {
        Some(end)
    }
}

pub fn split_off_str(mut key: Vec<u8>, end: usize) -> (String, Vec<u8>) {
    // try:
    // 1) valid utf-8 string
    // 2) valid AccountId bytes
    // 3) hex encode
    let prefix = string_account_or_hex(&key[2..end]);
    key.splice(0..end, []);
    (prefix, key)
}

// If we can't split, we return empty module
pub fn split_module(key: Vec<u8>) -> (String, Vec<u8>) {
    // If no cut point, we use "" as module
    match cut_point(&key) {
        Some(end) => split_off_str(key, end),
        None => ("".to_string(), key),
    }
}

pub fn split_bucket(key: Vec<u8>) -> (String, Vec<u8>) {
    match cut_point(&key) {
        Some(end) => split_off_str(key, end),
        None => (String::from_utf8(key).unwrap(), vec![]),
    }
}

// TODO: add some tests here
