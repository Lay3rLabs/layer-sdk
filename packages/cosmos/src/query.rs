use bytes::Bytes;
use cosmwasm_std::Event;
use layer_app::encode_cosmwasm_response;
use layer_std::api::{GasInfo, TxResponse, TxResult};
use layer_std::response::{
    AccountResponse, AuthQueryResponse, BankQueryResponse, QueryResponse, WasmQueryResponse,
};
use layer_std::{AccountId, AuthQuery, BankQuery, Query, QueryError, WasmQuery};

use cosmos_sdk_proto::cosmos::auth::v1beta1::{
    BaseAccount, QueryAccountRequest, QueryAccountResponse,
};
use cosmos_sdk_proto::cosmos::bank::v1beta1::{
    QueryAllBalancesRequest, QueryAllBalancesResponse, QueryBalanceRequest, QueryBalanceResponse,
    QuerySpendableBalancesRequest, QuerySupplyOfRequest, QuerySupplyOfResponse,
    QueryTotalSupplyRequest, QueryTotalSupplyResponse,
};
use cosmos_sdk_proto::cosmos::tx::v1beta1::{SimulateRequest, SimulateResponse};
use cosmos_sdk_proto::cosmwasm::wasm::v1::{
    AbsoluteTxPosition, QueryCodeRequest, QueryCodeResponse, QueryCodesRequest, QueryCodesResponse,
    QueryContractInfoRequest, QueryContractInfoResponse, QueryContractsByCodeRequest,
    QueryContractsByCodeResponse, QueryRawContractStateRequest, QueryRawContractStateResponse,
    QuerySmartContractStateRequest, QuerySmartContractStateResponse,
};
use cosmos_sdk_proto::prost::Message;
use cosmos_sdk_proto::traits::MessageExt;

use crate::pubkey::encode_cosmos_pubkey;
use crate::tx::FIXED_ACCOUNT_NUMBER;
use crate::utils::{encode_sdk_coin, encode_sdk_coins};
use crate::{parse_cosmos_tx, CosmosError};

/// "/app" prefix for special application queries
/// /app/version returns app version string cast to bytes
pub const QUERY_PATH_APP: &str = "app";

/// /store/{substore}/{key} returns ??
/// Check out CMS queryable interface
/// https://github.com/cosmos/cosmos-sdk/blob/v0.47.2/baseapp/abci.go#L920-L941
pub const QUERY_PATH_STORE: &str = "store";

// These two exist in Cosmos SDK, but we don't use them
// const QUERY_PATH_CUSTOM: &str = "custom";
// const QUERY_PATH_P2P: &str = "p2p";

// See relevant code we emulate at https://github.com/cosmos/cosmos-sdk/blob/v0.47.2/baseapp/abci.go#L538-L561
pub fn parse_cosmos_query(path: &str, data: Bytes, chain_id: &str) -> Result<Query, QueryError> {
    if let Some(grpc_res) = parse_cosmos_grpc_query(path, data.clone(), chain_id)? {
        return Ok(grpc_res);
    }

    // FIXME: use cleaner match
    // try some special cases
    let fragments: Vec<&str> = path.split('/').collect();
    if fragments.len() < 2 {
        return Err(QueryError::UnsupportedPath(path.to_string()));
    }
    match fragments[1] {
        QUERY_PATH_APP => {
            if fragments.len() != 2 {
                return Err(QueryError::UnsupportedPath(path.to_string()));
            }
            parse_app_query(fragments[2], data, chain_id)
        }
        QUERY_PATH_STORE => parse_store_query(&fragments[2..], data),
        _ => Err(QueryError::UnsupportedPath(path.to_string())),
    }
}

/// for raw queries
fn parse_store_query(_fragments: &[&str], data: Bytes) -> Result<Query, QueryError> {
    // FIXME: review if this is correct when we have a sample caller for compatibility
    Ok(Query::Raw { key: data.into() })
}

/// simulate and version support
fn parse_app_query(command: &str, data: Bytes, chain_id: &str) -> Result<Query, QueryError> {
    match command {
        "simulate" => {
            // FIXME: error handling is ugly, revise proper types
            let tx = parse_cosmos_tx(data, chain_id)
                .map_err(|e| QueryError::ParseError(e.to_string()))?;
            Ok(Query::Simulate(tx))
        }
        "version" => todo!(),
        _ => Err(QueryError::UnsupportedPath(format!(
            "{}/{}",
            QUERY_PATH_APP, command
        ))),
    }
}

/// This will use grpc path lookups, returns Ok(None) if not a match, so we try special queries
fn parse_cosmos_grpc_query(
    path: &str,
    data: Bytes,
    chain_id: &str,
) -> Result<Option<Query>, QueryError> {
    // FIXME: add auth queries
    // FIXME: make more extensible when we add cosmwasm, etc support
    match path {
        // see https://github.com/cosmos/cosmos-rust/blob/main/cosmos-sdk-proto/src/prost/cosmos-sdk/cosmos.bank.v1beta1.tonic.rs#L85
        "/cosmos.bank.v1beta1.Query/Balance" => {
            let req = QueryBalanceRequest::decode(data).map_err(CosmosError::from)?;
            let address = AccountId::parse_string(&req.address)?;
            let denom = req.denom;
            let query = BankQuery::Balance { address, denom };
            Ok(Some(query.into()))
        }
        "/cosmos.bank.v1beta1.Query/AllBalances" => {
            let req = QueryAllBalancesRequest::decode(data).map_err(CosmosError::from)?;
            let address = AccountId::parse_string(&req.address)?;
            let query = BankQuery::AllBalances { address };
            Ok(Some(query.into()))
        }
        "/cosmos.bank.v1beta1.Query/SpendableBalances" => {
            let req = QuerySpendableBalancesRequest::decode(data).map_err(CosmosError::from)?;
            let address = AccountId::parse_string(&req.address)?;
            let query = BankQuery::AllBalances { address };
            Ok(Some(query.into()))
        }
        "/cosmos.bank.v1beta1.Query/TotalSupply" => {
            let _req = QueryTotalSupplyRequest::decode(data).map_err(CosmosError::from)?;
            let query = BankQuery::TotalSupply {};
            Ok(Some(query.into()))
        }
        "/cosmos.bank.v1beta1.Query/SupplyOf" => {
            let req = QuerySupplyOfRequest::decode(data).map_err(CosmosError::from)?;
            let denom = req.denom;
            let query = BankQuery::Supply { denom };
            Ok(Some(query.into()))
        }
        "/cosmos.auth.v1beta1.Query/Account" => {
            let req = QueryAccountRequest::decode(data).map_err(CosmosError::from)?;
            let address = AccountId::parse_string(&req.address)?;
            let query = AuthQuery::Account { address };
            Ok(Some(query.into()))
        }
        "/cosmos.tx.v1beta1.Service/Simulate" => {
            let req = SimulateRequest::decode(data).map_err(CosmosError::from)?;
            let tx = parse_cosmos_tx(req.tx_bytes.into(), chain_id)
                .map_err(|e| QueryError::ParseError(e.to_string()))?;
            Ok(Some(Query::Simulate(tx)))
        }
        "/cosmwasm.wasm.v1.Query/ContractInfo" => {
            let req = QueryContractInfoRequest::decode(data).map_err(CosmosError::from)?;
            let contract_addr = AccountId::parse_string(&req.address)?;
            let query = WasmQuery::ContractInfo { contract_addr };
            Ok(Some(query.into()))
        }
        "/cosmwasm.wasm.v1.Query/ContractsByCode" => {
            let req = QueryContractsByCodeRequest::decode(data).map_err(CosmosError::from)?;
            // TODO: care about pagination
            let code_id = req.code_id;
            let query = WasmQuery::ContractsByCode { code_id };
            Ok(Some(query.into()))
        }
        "/cosmwasm.wasm.v1.Query/RawContractState" => {
            let req = QueryRawContractStateRequest::decode(data).map_err(CosmosError::from)?;
            let contract_addr = AccountId::parse_string(&req.address)?;
            let query = WasmQuery::Raw {
                contract_addr,
                key: req.query_data.into(),
            };
            Ok(Some(query.into()))
        }
        "/cosmwasm.wasm.v1.Query/SmartContractState" => {
            let req = QuerySmartContractStateRequest::decode(data).map_err(CosmosError::from)?;
            let contract_addr = AccountId::parse_string(&req.address)?;
            let query = WasmQuery::Smart {
                contract_addr,
                msg: req.query_data.into(),
            };
            Ok(Some(query.into()))
        }
        "/cosmwasm.wasm.v1.Query/Code" => {
            let req = QueryCodeRequest::decode(data).map_err(CosmosError::from)?;
            let query = WasmQuery::CodeInfo {
                code_id: req.code_id,
                include_wasm: true, // consistent with the sdk format
            };
            Ok(Some(query.into()))
        }
        "/cosmwasm.wasm.v1.Query/Codes" => {
            let req = QueryCodesRequest::decode(data).map_err(CosmosError::from)?;
            let query = WasmQuery::ListCodes {
                from: req.pagination.as_ref().map(|p| p.offset),
                limit: req.pagination.as_ref().map(|p| p.limit as u32),
            };
            Ok(Some(query.into()))
        }
        // "/cosmos.auth.v1beta1.Query/Accounts" => {
        //     let _ = QueryAccountsRequest::decode(data).map_err(CosmosError::from)?;
        //     unimplemented!();
        // }
        // "/cosmos.auth.v1beta1.Query/Params" => {
        //     let _ = QueryParamsRequest::decode(data).map_err(CosmosError::from)?;
        //     unimplemented!();
        // }
        _ => Ok(None),
    }
}

pub fn encode_cosmos_response<E: std::error::Error>(
    res: QueryResponse<E>,
) -> Result<Vec<u8>, QueryError> {
    match res {
        QueryResponse::Raw { key: _, value } => Ok(value),
        QueryResponse::Auth(auth) => encode_auth_response(auth),
        QueryResponse::Bank(bank) => Ok(encode_bank_response(bank)),
        QueryResponse::Simulate(simulate) => encode_simulate_response(simulate),
        QueryResponse::Wasm(wasm) => Ok(encode_wasm_response(wasm)),
    }
}

pub fn encode_auth_response(res: AuthQueryResponse) -> Result<Vec<u8>, QueryError> {
    match res {
        AuthQueryResponse::Account(acc) => {
            let (address, pub_key, sequence) = match acc {
                AccountResponse::External {
                    address,
                    pubkey,
                    sequence,
                } => {
                    let pub_key = pubkey.as_ref().map(encode_cosmos_pubkey).transpose()?;
                    (address.to_string(), pub_key, sequence)
                }
                AccountResponse::Internal { address } => (address.to_string(), None, 0),
                AccountResponse::Smart { address, .. } => (address.to_string(), None, 0),
            };
            let base = BaseAccount {
                address,
                account_number: FIXED_ACCOUNT_NUMBER,
                sequence,
                pub_key,
            };
            let account = Some(
                base.to_any()
                    .map_err(|_| QueryError::EncodingError("to_any".to_string()))?,
            );
            Ok(QueryAccountResponse { account }.encode_to_vec())
        }
    }
}

pub fn encode_bank_response(res: BankQueryResponse) -> Vec<u8> {
    match res {
        BankQueryResponse::Balance(r) => {
            let balance = Some(encode_sdk_coin(&r.amount));
            QueryBalanceResponse { balance }.encode_to_vec()
        }
        BankQueryResponse::AllBalances(r) => QueryAllBalancesResponse {
            balances: encode_sdk_coins(&r.amount),
            pagination: None,
        }
        .encode_to_vec(),
        BankQueryResponse::Supply(r) => {
            let amount = Some(encode_sdk_coin(&r.amount));
            QuerySupplyOfResponse { amount }.encode_to_vec()
        }
        BankQueryResponse::TotalSupply(r) => {
            let supply = r.amounts.iter().map(encode_sdk_coin).collect();
            QueryTotalSupplyResponse {
                supply,
                pagination: None,
            }
            .encode_to_vec()
        }
    }
}

pub fn encode_simulate_response<E: std::error::Error>(
    result: TxResult<E>,
) -> Result<Vec<u8>, QueryError> {
    let gas_info = encode_gas_info(&result.gas);
    // if the result was an error, we just return error to the query service, it gets encoded at rpc level
    let r = result
        .result
        .as_ref()
        .map_err(|e| QueryError::EncodingError(e.to_string()))?;
    let result = encode_tx_result(r);

    let sim = SimulateResponse {
        gas_info: Some(gas_info),
        result: Some(result),
    };
    Ok(sim.encode_to_vec())
}

pub fn encode_wasm_response(res: WasmQueryResponse) -> Vec<u8> {
    match res {
        WasmQueryResponse::Smart(data) => {
            QuerySmartContractStateResponse { data: data.into() }.encode_to_vec()
        }
        WasmQueryResponse::Raw(data) => {
            QueryRawContractStateResponse { data: data.into() }.encode_to_vec()
        }
        WasmQueryResponse::CodeInfo(code) => QueryCodeResponse {
            data: code.data.into(),
            code_info: Some(cosmos_sdk_proto::cosmwasm::wasm::v1::CodeInfoResponse {
                code_id: code.code_info.code_id,
                creator: code.code_info.creator.to_string(),
                data_hash: code.code_info.checksum.into(),
                instantiate_permission: None,
            }),
        }
        .encode_to_vec(),
        WasmQueryResponse::ListCodes(codes) => QueryCodesResponse {
            code_infos: codes
                .code_infos
                .into_iter()
                .map(
                    |code| cosmos_sdk_proto::cosmwasm::wasm::v1::CodeInfoResponse {
                        code_id: code.code_id,
                        creator: code.creator.to_string(),
                        data_hash: code.checksum.into(),
                        instantiate_permission: None,
                    },
                )
                .collect(),
            pagination: None, // TODO
        }
        .encode_to_vec(),
        WasmQueryResponse::ContractInfo(info) => QueryContractInfoResponse {
            address: info.addresss.to_string(),
            contract_info: Some(cosmos_sdk_proto::cosmwasm::wasm::v1::ContractInfo {
                code_id: info.code_id,
                creator: info.creator.to_string(),
                admin: info.admin.map(|s| s.to_string()).unwrap_or_default(),
                label: info.label,
                ibc_port_id: info.ibc_port.unwrap_or_default(),
                created: Some(AbsoluteTxPosition {
                    block_height: info.created,
                    tx_index: 0,
                }),
                extension: None,
            }),
        }
        .encode_to_vec(),
        WasmQueryResponse::ContractsByCode(c) => QueryContractsByCodeResponse {
            contracts: c.contracts.iter().map(|x| x.to_string()).collect(),
            pagination: None, // TODO
        }
        .encode_to_vec(),
    }
}

fn encode_gas_info(gas: &GasInfo) -> cosmos_sdk_proto::cosmos::base::abci::v1beta1::GasInfo {
    cosmos_sdk_proto::cosmos::base::abci::v1beta1::GasInfo {
        gas_wanted: gas.gas_wanted,
        gas_used: gas.gas_used,
    }
}

pub fn encode_tx_result(
    response: &TxResponse,
) -> cosmos_sdk_proto::cosmos::base::abci::v1beta1::Result {
    let combined_data = msg_data_to_proto(response.data.clone());

    cosmos_sdk_proto::cosmos::base::abci::v1beta1::Result {
        data: combined_data,
        events: response
            .events
            .iter()
            .cloned()
            .flat_map(|e| e.into_iter().map(encode_cosmos_event))
            .collect(),
        log: "".to_string(),
    }
}

pub fn msg_data_to_proto(data: Vec<layer_std::MsgData>) -> Vec<u8> {
    let data = data
        .into_iter()
        .map(|m| {
            let (msg_type, data) = encode_cosmwasm_response(m);
            cosmos_sdk_proto::cosmos::base::abci::v1beta1::MsgData {
                msg_type: msg_type.to_string(),
                data,
            }
        })
        .collect();
    cosmos_sdk_proto::cosmos::base::abci::v1beta1::TxMsgData { data }.encode_to_vec()
}

pub fn encode_cosmos_event(event: Event) -> cosmos_sdk_proto::tendermint::abci::Event {
    let attributes = event
        .attributes
        .into_iter()
        .map(|a| cosmos_sdk_proto::tendermint::abci::EventAttribute {
            key: a.key,
            value: a.value,
            index: true,
        })
        .collect();
    cosmos_sdk_proto::tendermint::abci::Event {
        r#type: event.ty,
        attributes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hex_literal::hex;

    #[test]
    #[ignore = "need to get new data with 0x address"]
    fn parse_simulate() {
        let path = "/cosmos.tx.v1beta1.Service/Simulate";
        let data = hex!("1282020AAB010A91010A1C2F636F736D6F732E62616E6B2E763162657461312E4D736753656E6412710A2D736C6179337231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D766B3372336A122D736C6179337231757A3479303579387366736D75657A61616B70706D68326470636667797A72716439723063611A110A067570756C7365120732303030303030121555736520796F757220706F77657220776973656C7912500A4C0A460A1F2F636F736D6F732E63727970746F2E736563703235366B312E5075624B657912230A21034F04181EEBA35391B858633A765C4A0C189697B40D216354D50890D350C7029012020A0012001A00");
        let chain_id = "slay3r-dev-1";

        let tx = parse_cosmos_query(path, Bytes::from(data.to_vec()), chain_id).unwrap();
        assert!(matches!(tx, Query::Simulate(_)));
    }
}
