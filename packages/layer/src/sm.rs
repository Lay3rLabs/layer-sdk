use tracing::{
    debug_span,
    field::{debug, display, Empty},
    info_span, trace_span,
};

use cosmwasm_std::{BlockInfo, Event, StdError};
use layer_std::api::{Block, GasInfo, MsgResponse, TxResponse, TxResult};
use layer_std::response::QueryResponse;
use layer_std::{AccountId, GasMeter, Msg, Query, Tx};
use layer_storage::{AppMeter, ReadonlyStorage, ScratchTx, Storage};

use crate::bank::Bank;
use crate::error::{PulsarError, PulsarResult};
use crate::genesis::GenesisState;
use crate::wasm::Wasm;
use crate::{
    auth::{Auth, TxData},
    wasm::WasmConfig,
};

/// This is an immutable State Machine logic that processes incoming transactions.
/// All mutable state held in Storage, which is passed as an argument to these methods.
#[derive(Debug)]
pub struct StateMachine {
    pub auth: Auth,
    pub bank: Bank,
    pub wasm: Wasm,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub wasm: WasmConfig,
}

impl AppConfig {
    // (Temporary?) helper to construct with important fields filled
    pub fn new(cache_dir: &str) -> AppConfig {
        AppConfig {
            wasm: WasmConfig {
                cache_dir: cache_dir.to_string(),
            },
        }
    }
}

impl StateMachine {
    pub fn new(config: &AppConfig) -> Self {
        StateMachine {
            auth: Auth::new(),
            bank: Bank::new(),
            wasm: Wasm::new(&config.wasm),
        }
    }

    // TODO: similar call for "load_from_storage" to let them rehydrate on start
    pub fn init(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        genesis: GenesisState,
    ) -> PulsarResult<()> {
        info_span!("sm.init", ?genesis);
        self.bank.init(storage, meter, block, genesis.bank, self)?;
        self.wasm.init(storage, meter, block, genesis.wasm, self)?;
        Ok(())
    }

    pub fn query(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        block: &BlockInfo,
        request: Query,
    ) -> Result<QueryResponse<PulsarError>, PulsarError> {
        let _span = trace_span!("sm.query").entered();
        let result = match request {
            Query::Raw { key } => {
                let value = storage
                    .get(meter, &key)?
                    .ok_or_else(|| StdError::not_found("raw"))?;
                Ok(QueryResponse::Raw { key, value })
            }
            Query::Bank(bank) => self.bank.query(storage, meter, block, self, bank),
            Query::Auth(auth) => self.auth.query(storage, meter, block, self, auth),
            Query::Simulate(tx) => {
                // based on execute_tx
                let mut store = ScratchTx::new(storage);
                let result = self.query_simulate(&mut store, meter, block, tx);
                let gas = GasInfo::from_meter(meter);
                let res = TxResult { gas, result };
                Ok(QueryResponse::Simulate(res))
            }
            Query::Wasm(wasm) => self.wasm.query(storage, meter, block, self, wasm),
        };
        result
    }

    fn query_simulate(
        &self,
        store: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        tx: Tx,
    ) -> PulsarResult<TxResponse> {
        let _span = debug_span!("sm.query_simulate").entered();
        let tx_byte_gas = tx.tx_len() * crate::app::GAS_COST_TX_BYTE;
        let data =
            self.auth
                .validate_tx(&mut AppMeter::new(store), meter, block, self, tx, false)?;
        // It's roughly 6000 gas to transfer fees, which is not done in simulate.
        // We charge here to make sure estimates are good.
        meter.charge(6000)?;

        // we also need to charge for length gas here (like in execute_tx)
        meter.charge(tx_byte_gas)?;

        let resps = data
            .msgs
            .into_iter()
            .map(|msg| self.process_msg(store, meter, &data.signer, block, msg))
            .collect::<PulsarResult<Vec<_>>>()?;
        // Question: pull this out to a function? (copied from execute_tx)
        let data = resps.iter().map(|r| r.data.clone()).collect();
        let events = resps.into_iter().map(|r| r.events).collect();
        tracing::debug!(gas = meter.used(), "simulated");
        Ok(TxResponse { data, events })
    }

    pub fn process_msg(
        &self,
        storage: &mut dyn Storage,
        gas: &GasMeter,
        sender: &AccountId,
        block: &BlockInfo,
        msg: Msg,
    ) -> PulsarResult<MsgResponse> {
        let span = debug_span!("sm.process_msg", ?msg, success = Empty, error = Empty).entered();
        let res = match msg {
            Msg::Bank(bank) => {
                let mut metered = AppMeter::new(storage);
                self.bank
                    .process_msg(&mut metered, gas, block, self, sender, bank)
            }
            Msg::Wasm(wasm) => {
                let mut metered = AppMeter::new(storage);
                self.wasm
                    .process_msg(&mut metered, gas, block, self, sender, wasm)
            }
        };
        match &res {
            Ok(response) => span.record("success", debug(&response.events)),
            Err(error) => span.record("error", display(error)),
        };
        res
    }

    pub fn validate_tx(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        tx: Tx,
    ) -> PulsarResult<TxData> {
        let mut metered = AppMeter::new(storage);
        self.auth
            .validate_tx(&mut metered, meter, block, self, tx, true)
    }

    /// Note: erroring here (including exceeding gas limits) will abort block execution. Be careful.
    pub fn begin_block(
        &self,
        _storage: &mut dyn Storage,
        // this is set to the gas limit for begin blockers
        _meter: &GasMeter,
        // here we have full block info including proposer and voters (for rewards if needed)
        _block: &Block,
    ) -> PulsarResult<Vec<Event>> {
        // FIXME: implement this later
        Ok(vec![])
    }

    /// Note: erroring here (including exceeding gas limits) will abort block execution. Be careful.
    pub fn end_block(
        &self,
        _storage: &mut dyn Storage,
        // this is set to the gas limit for end blockers
        _meter: &GasMeter,
        // this is just block metadata
        _block: &BlockInfo,
    ) -> PulsarResult<Vec<Event>> {
        // FIXME: implement this later
        Ok(vec![])
    }
}
