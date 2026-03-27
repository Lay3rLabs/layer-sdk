use abstract_cw_multi_test::AppResponse;

use cosmwasm_std::StdError;
use cw_orch_core::environment::{NodeQuerier, Querier};
use layer_std::{response::QueryResponse, Query, QueryError};

use crate::Slay3rGolem;

pub struct Slay3rNode {
    golem: Slay3rGolem,
}

impl Slay3rNode {
    pub fn new(golem: &Slay3rGolem) -> Self {
        Self {
            golem: golem.clone(),
        }
    }
}

impl Querier for Slay3rNode {
    type Error = layer_app::PulsarError;
}

impl NodeQuerier for Slay3rNode {
    type Response = AppResponse;

    fn latest_block(&self) -> Result<cosmwasm_std::BlockInfo, Self::Error> {
        Ok(self.golem.block_info())
    }

    fn block_height(&self) -> Result<u64, Self::Error> {
        let info = self.golem.block_info();
        Ok(info.height)
    }

    fn block_time(&self) -> Result<u128, Self::Error> {
        let info = self.golem.block_info();
        Ok(info.time.nanos().into())
    }

    fn simulate_tx(&self, tx_bytes: Vec<u8>) -> Result<u64, Self::Error> {
        let tx = layer_cosmos::parse_cosmos_tx(tx_bytes.into(), &self.golem.config.chain_id)
            .map_err(|e| QueryError::ParseError(e.to_string()))?;
        let query = Query::Simulate(tx);

        let app: std::cell::Ref<layer_app::App<layer_storage::MemoryStore>> =
            self.golem.app.borrow();
        let res = app.query(query)?;
        match res {
            QueryResponse::Simulate(r) => {
                // Return an error if execution failed
                let _ = r.result?;
                // Otherwise return gas used
                Ok(r.gas.gas_used)
            }
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }

    fn block_by_height(&self, _height: u64) -> Result<cosmwasm_std::BlockInfo, Self::Error> {
        unimplemented!("Not supported")
    }

    fn find_tx(&self, _hash: String) -> Result<Self::Response, Self::Error> {
        unimplemented!("Not supported")
    }
}
