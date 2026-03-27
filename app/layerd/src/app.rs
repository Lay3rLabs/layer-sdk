use bytes::Bytes;
use core::panic;
use parking_lot::{RwLock, RwLockReadGuard};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{
    debug_span,
    field::{display, Empty},
    info, info_span, trace,
};

use layer_abci::Application;
use tendermint_proto::abci::{
    response_process_proposal, RequestApplySnapshotChunk, RequestCheckTx, RequestEcho,
    RequestFinalizeBlock, RequestInfo, RequestInitChain, RequestLoadSnapshotChunk,
    RequestOfferSnapshot, RequestPrepareProposal, RequestProcessProposal, RequestQuery,
    ResponseApplySnapshotChunk, ResponseCheckTx, ResponseCommit, ResponseEcho,
    ResponseFinalizeBlock, ResponseFlush, ResponseInfo, ResponseInitChain, ResponseListSnapshots,
    ResponseLoadSnapshotChunk, ResponseOfferSnapshot, ResponsePrepareProposal,
    ResponseProcessProposal, ResponseQuery,
};

use layer_app::{App, AppConfig, AppLoadError, StateMachine, SyncProvider};
use layer_std::{api::TxResult, HexEncode};
use layer_storage::PersistentStorage;

use crate::{
    decode::{
        check_response_to_proto, finalize_response_to_proto, init_response_to_proto,
        query_response_to_proto,
    },
    encode::{
        check_request_from_proto, finalize_request_from_proto, init_request_from_proto,
        query_request_from_proto,
    },
};

type Tx = bytes::Bytes;

#[derive(Debug)]
pub struct Pulsarium<T: PersistentStorage + 'static> {
    // Applications are `Send` + `Clone` + `'static` because they are cloned for
    // each incoming connection to the ABCI [`Server`]. It is up to the
    // application developer to manage shared state between these clones of their
    // application.
    app: Arc<RwLock<App<T>>>,

    mempool: Arc<RwLock<Vec<Tx>>>,
}

impl<T: PersistentStorage + 'static> Clone for Pulsarium<T> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            mempool: self.mempool.clone(),
        }
    }
}

unsafe impl<T: PersistentStorage> Send for Pulsarium<T> {}

impl<T: PersistentStorage + 'static> Pulsarium<T> {
    /// Creates a new app and tries to load state from storage.
    /// If the storage is empty, the app will be left in an uninitialized state
    /// and init_chain must be called before any other method.
    pub fn new(store: T, config: AppConfig) -> Self {
        let logic = StateMachine::new(&config);
        let mut app = App::new(store, logic);
        match app.load_from_storage() {
            Ok(_) => {
                let height = app.info().unwrap().height;
                info!(height, "Initialized app from storage");
            }
            Err(AppLoadError::NoStoredState) => {
                info!("No stored state, app is uninitialized");
            }
            Err(e) => panic!("Error loading app from storage: {}", e),
        };

        Self {
            app: Arc::new(RwLock::new(app)),
            mempool: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn sync(&self) -> RwLockReadGuard<impl SyncProvider> {
        self.app.as_ref().read()
    }
}

impl<T: PersistentStorage + 'static> Application for Pulsarium<T> {
    fn echo(&self, request: RequestEcho) -> ResponseEcho {
        trace!("abci echo");
        ResponseEcho {
            message: request.message,
        }
    }

    /// Provide information about the ABCI application.
    fn info(&self, _request: RequestInfo) -> ResponseInfo {
        let _span = debug_span!("abci_info").entered();
        let app = self.app.read();
        let block = app.info();
        let app_hash = app.app_hash();
        let last_block_height = block.map(|b| b.height).unwrap_or(0) as i64;
        ResponseInfo {
            data: format!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION")),
            version: "".to_string(),
            app_version: 1,    // FIXME: what to put here?
            last_block_height, // ugly api :(
            last_block_app_hash: app_hash.into(),
        }
    }

    /// Called once upon genesis.
    fn init_chain(&self, request: RequestInitChain) -> ResponseInitChain {
        let _span =
            info_span!("abci_init_chain", initial_height = request.initial_height).entered();
        let request = init_request_from_proto(request);
        // This requires we are in WaitingInit state, otherwise panic
        let res = self.app.write().init(request).unwrap();
        init_response_to_proto(res)
    }

    /// Query the application for data at the current or past height.
    fn query(&self, request: RequestQuery) -> ResponseQuery {
        let span = debug_span!(
            "abci_query", 
            raw_request.path = request.path,
            raw_request.data = %HexEncode::new(&request.data),
            raw_response.code = Empty,
            raw_response.log = Empty,
            raw_response.key = Empty,
            raw_response.value = Empty)
        .entered();
        let app = self.app.read();
        let chain_id = app.chain_id();
        let height = app.info().map(|i| i.height).unwrap_or(0);
        let request = query_request_from_proto(request, chain_id);
        let out = match request {
            Ok(r) => {
                let res = app.query(r);
                query_response_to_proto(res, height)
            }
            Err(e) => e,
        };

        //Add response into to the same span
        span.record("raw_response.code", out.code);
        span.record("raw_response.log", &out.log);
        span.record("raw_response.key", display(HexEncode::new(&out.key)));
        span.record("raw_response.value", display(HexEncode::new(&out.value)));
        out
    }

    /// Check the given transaction before putting it into the local mempool.
    fn check_tx(&self, request: RequestCheckTx) -> ResponseCheckTx {
        let hash = tx_hash(&request.tx);
        // TODO: bump to info if the check_tx failed?
        // debug should give enough info to debug a failed tx in detail
        // trace would have info for optimizing
        let span = debug_span!("abci_check_tx",
            raw_tx = %HexEncode::new(&request.tx),
            tx_hash = %HexEncode::new(&hash),
            gas_wanted = Empty,
            gas_used = Empty,
            code = Empty,
            log = Empty,
        )
        .entered();

        let app = self.app.read();
        let chain_id = app.chain_id();
        let to_check = match check_request_from_proto(request, chain_id) {
            Ok(tx) => tx,
            Err(e) => {
                return check_response_to_proto(TxResult::failure(e.into()));
            }
        };
        let res = app.check_tx(to_check);

        // Really no easier way to release the app lock??
        parking_lot::lock_api::RwLockReadGuard::unlock_fair(app);

        let out = check_response_to_proto(res);
        span.record("gas_wanted", out.gas_wanted);
        span.record("gas_used", out.gas_used);
        span.record("code", out.code);
        span.record("log", &out.log);
        out
    }

    fn finalize_block(&self, request: RequestFinalizeBlock) -> ResponseFinalizeBlock {
        // Abel: here we add the height and the block hash
        let _span = info_span!("abci_finalize_block", height = request.height, block_hash = %HexEncode::new(&request.hash)).entered();
        info!("Finalize Block");
        let mut app = self.app.write();
        let chain_id = app.chain_id();
        let request = finalize_request_from_proto(request, chain_id);

        // FIXME: crash node on finalize block error?
        let res = app.finalize_block(request).unwrap();

        finalize_response_to_proto(res)
    }

    /// Signals that messages queued on the client should be flushed to the server.
    fn flush(&self) -> ResponseFlush {
        ResponseFlush {}
    }

    /// Commit the current state at the current height.
    fn commit(&self) -> ResponseCommit {
        // See explanation here: https://github.com/cometbft/cometbft/blob/main/spec/abci/abci%2B%2B_basic_concepts.md#method-overview
        // Always return retain_height = 0, so we never lose tendermint blocks
        ResponseCommit { retain_height: 0 }
    }

    /// Used during state sync to discover available snapshots on peers.
    fn list_snapshots(&self) -> ResponseListSnapshots {
        // TODO: implement... make snapshot functions all info, so obvious if they are called somehow
        info!("ABCI list_snapshots");
        Default::default()
    }

    /// Called when bootstrapping the node using state sync.
    fn offer_snapshot(&self, _request: RequestOfferSnapshot) -> ResponseOfferSnapshot {
        info!("ABCI offer_snapshot");
        Default::default()
    }

    /// Used during state sync to retrieve chunks of snapshots from peers.
    fn load_snapshot_chunk(&self, _request: RequestLoadSnapshotChunk) -> ResponseLoadSnapshotChunk {
        info!("ABCI load_snapshot_chunk");
        Default::default()
    }

    /// Apply the given snapshot chunk to the application's state.
    fn apply_snapshot_chunk(
        &self,
        _request: RequestApplySnapshotChunk,
    ) -> ResponseApplySnapshotChunk {
        info!("ABCI apply_snapshot_chunk");
        Default::default()
    }

    /// A stage where the application can modify the list of transactions
    /// in the preliminary proposal.
    ///
    /// The default implementation implements the required behavior in a
    /// very naive way, removing transactions off the end of the list
    /// until the limit on the total size of the transaction is met as
    /// specified in the `max_tx_bytes` field of the request, or there are
    /// no more transactions. It's up to the application to implement
    /// more elaborate removal strategies.
    ///
    /// This method is introduced in ABCI++.
    fn prepare_proposal(&self, request: RequestPrepareProposal) -> ResponsePrepareProposal {
        // take txs out of mempool
        let span = info_span!(
            "prepare_proposal",
            request_txs = request.txs.len(),
            mempool_txs = Empty
        )
        .entered();

        let mut txs = request.txs;

        // Per the ABCI++ spec: if the size of RequestPrepareProposal.txs is
        // greater than RequestPrepareProposal.max_tx_bytes, the Application
        // MUST remove transactions to ensure that the
        // RequestPrepareProposal.max_tx_bytes limit is respected by those
        // transactions returned in ResponsePrepareProposal.txs.
        let RequestPrepareProposal { max_tx_bytes, .. } = request;
        let max_tx_bytes: usize = max_tx_bytes.try_into().unwrap_or(0);
        let mut total_tx_bytes: usize = txs.iter().map(|tx| tx.len()).sum();
        // FIXME: we could do some prioritization based on gas price, right now just FIFO
        while total_tx_bytes > max_tx_bytes {
            if let Some(tx) = txs.pop() {
                total_tx_bytes = total_tx_bytes.saturating_sub(tx.len());
            } else {
                break;
            }
        }

        span.record("mempool_txs", txs.len());
        ResponsePrepareProposal { txs }
    }

    /// A stage where the application can accept or reject the proposed block.
    ///
    /// The default implementation returns the status value of `ACCEPT`.
    ///
    /// This method is introduced in ABCI++.
    fn process_proposal(&self, _request: RequestProcessProposal) -> ResponseProcessProposal {
        ResponseProcessProposal {
            status: response_process_proposal::ProposalStatus::Accept as i32,
        }
    }
}

// TODO: pull out tx hash elsewhere
fn tx_hash(tx: &Bytes) -> Vec<u8> {
    Sha256::digest(tx).to_vec()
}
