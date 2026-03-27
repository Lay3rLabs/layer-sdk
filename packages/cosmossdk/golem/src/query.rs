mod bank;
mod node;
mod wasm;

use layer_app::PulsarError;

use layer_std::response::{AuthQueryResponse, QueryResponse};
use layer_std::{AccountId, AuthQuery};
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;

use cosmwasm_std::{Addr, BlockInfo, Coin, StdError};

use cw_orch_core::environment::EnvironmentInfo;
use cw_orch_core::environment::{
    BankQuerier, DefaultQueriers, EnvironmentQuerier, NodeQuerier, Querier, QuerierGetter,
    QueryHandler, WasmQuerier,
};

use crate::Slay3rGolem;
use bank::Slay3rBank;
use node::Slay3rNode;
use wasm::Slay3rWasm;

// More queries can be added here
impl Slay3rGolem {
    pub fn get_sequence(&self, address: impl Into<String>) -> Result<u64, PulsarError> {
        let app = self.app.borrow();
        let address = AccountId::parse_string(&address.into())?;
        let query = AuthQuery::Account { address };
        let res = app.query(query.into())?;
        match res {
            QueryResponse::Auth(AuthQueryResponse::Account(r)) => Ok(r.external_sequence()),
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }
}

impl QueryHandler for Slay3rGolem {
    type Error = layer_app::PulsarError;

    /// Wait for an amount of blocks.
    fn wait_blocks(&self, amount: u64) -> Result<(), Self::Error> {
        for _ in 0..amount {
            self.next_block()?;
        }
        Ok(())
    }

    /// Wait for an amount of seconds, we just check every block to see what is it
    fn wait_seconds(&self, secs: u64) -> Result<(), Self::Error> {
        let end = self.block_info().time.plus_seconds(secs);
        while self.block_info().time < end {
            self.next_block()?;
        }
        Ok(())
    }

    /// Run an empty block to advance
    fn next_block(&self) -> Result<(), Self::Error> {
        self.run_block(vec![])?;
        Ok(())
    }

    /// Return current block info see [`BlockInfo`].
    fn block_info(&self) -> Result<BlockInfo, PulsarError> {
        self.node_querier().latest_block()
    }

    fn balance(
        &self,
        address: impl Into<String>,
        denom: Option<String>,
    ) -> Result<Vec<Coin>, PulsarError> {
        self.bank_querier().balance(address, denom)
    }

    /// Send a QueryMsg to a contract.
    fn query<Q: Serialize + Debug, T: Serialize + DeserializeOwned>(
        &self,
        query_msg: &Q,
        contract_address: &Addr,
    ) -> Result<T, PulsarError> {
        self.wasm_querier().smart_query(contract_address, query_msg)
    }
}

impl DefaultQueriers for Slay3rGolem {
    type Bank = Slay3rBank;
    type Wasm = Slay3rWasm;
    type Node = Slay3rNode;
}

impl QuerierGetter<Slay3rBank> for Slay3rGolem {
    fn querier(&self) -> Slay3rBank {
        Slay3rBank::new(self)
    }
}

impl QuerierGetter<Slay3rNode> for Slay3rGolem {
    fn querier(&self) -> Slay3rNode {
        Slay3rNode::new(self)
    }
}

impl QuerierGetter<Slay3rWasm> for Slay3rGolem {
    fn querier(&self) -> Slay3rWasm {
        Slay3rWasm::new(self)
    }
}

impl EnvironmentQuerier for Slay3rGolem {
    fn env_info(&self) -> EnvironmentInfo {
        todo!()
    }
}

impl Querier for Slay3rGolem {
    type Error = layer_app::PulsarError;
}
