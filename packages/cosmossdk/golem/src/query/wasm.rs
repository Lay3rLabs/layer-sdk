use serde::{de::DeserializeOwned, Serialize};

use cosmwasm_std::{
    from_json, to_json_binary, Binary, CodeInfoResponse, ContractInfoResponse, HexBinary, StdError,
};

use cw_orch_core::contract::interface_traits::{ContractInstance, Uploadable};
use cw_orch_core::environment::{Querier, WasmQuerier};
use cw_orch_core::CwEnvError;
use layer_std::response::{QueryResponse, WasmQueryResponse};
use layer_std::{AccountId, WasmQuery};

use crate::Slay3rGolem;

pub struct Slay3rWasm {
    golem: Slay3rGolem,
}

impl Slay3rWasm {
    pub fn new(golem: &Slay3rGolem) -> Self {
        Self {
            golem: golem.clone(),
        }
    }
}

impl Querier for Slay3rWasm {
    type Error = layer_app::PulsarError;
}

impl WasmQuerier for Slay3rWasm {
    type Chain = Slay3rGolem;

    fn code_id_hash(&self, code_id: u64) -> Result<HexBinary, Self::Error> {
        let info = self.code(code_id)?;
        Ok(info.checksum)
    }

    fn contract_info(
        &self,
        address: impl Into<String>,
    ) -> Result<ContractInfoResponse, Self::Error> {
        let app = self.golem.app.borrow();
        let contract_addr = AccountId::parse_string(&address.into())?;
        let query = WasmQuery::ContractInfo { contract_addr };
        let res = app.query(query.into())?;
        match res {
            QueryResponse::Wasm(WasmQueryResponse::ContractInfo(r)) => Ok(r.into()),
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }

    fn raw_query(
        &self,
        address: impl Into<String>,
        query_keys: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        let app = self.golem.app.borrow();
        let contract_addr = AccountId::parse_string(&address.into())?;
        let query = WasmQuery::Raw {
            contract_addr,
            key: query_keys.into(),
        };
        let res = app.query(query.into())?;
        match res {
            QueryResponse::Wasm(WasmQueryResponse::Raw(r)) => Ok(r.into()),
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }

    fn smart_query<Q: Serialize, T: DeserializeOwned>(
        &self,
        address: impl Into<String>,
        query_msg: &Q,
    ) -> Result<T, Self::Error> {
        let app: std::cell::Ref<layer_app::App<layer_storage::MemoryStore>> =
            self.golem.app.borrow();
        let contract_addr = AccountId::parse_string(&address.into())?;
        let msg = to_json_binary(query_msg)?;
        let query = WasmQuery::Smart { contract_addr, msg };
        let res = app.query(query.into())?;
        match res {
            QueryResponse::Wasm(WasmQueryResponse::Smart(r)) => Ok(from_json(r)?),
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }

    fn code(&self, code_id: u64) -> Result<CodeInfoResponse, Self::Error> {
        let app = self.golem.app.borrow();
        let query = WasmQuery::CodeInfo {
            code_id,
            include_wasm: false,
        };
        let res = app.query(query.into())?;
        let res = match res {
            QueryResponse::Wasm(WasmQueryResponse::CodeInfo(r)) => r.code_info,
            _ => return Err(StdError::generic_err("unexpected response").into()),
        };
        let out = CodeInfoResponse::new(res.code_id, res.creator.to_string(), res.checksum.into());
        Ok(out)
    }

    /// Returns the checksum of the WASM file if the env supports it. Will re-upload every time if not supported.
    fn local_hash<T: Uploadable + ContractInstance<Self::Chain>>(
        &self,
        _contract: &T,
    ) -> Result<HexBinary, CwEnvError> {
        <T as Uploadable>::wasm(&crate::core::MOCK_CHAIN_INFO.into()).checksum()
    }

    fn instantiate2_addr(
        &self,
        code_id: u64,
        creator: impl Into<String>,
        salt: Binary,
    ) -> Result<String, Self::Error> {
        // load code hash / checksum from the chain
        let checksum: Binary = self.code(code_id)?.checksum.into();
        let creator = AccountId::parse_string(&creator.into())?;
        // Note: implementation ignores message part:
        // https://github.com/CosmWasm/cosmwasm/blob/v1.5.5/packages/std/src/addresses.rs#L349-L358
        // https://medium.com/cosmwasm/dev-note-3-limitations-of-instantiate2-and-how-to-deal-with-them-a3f946874230
        // Slay3r also does this inside WasmKeeper::process_msg (WasmMsg::Instantiate2 branch)
        let addr = layer_app::build_instantiate_2_address(&checksum, &creator, &salt, b"")?;
        Ok(addr.to_string())
    }
}
