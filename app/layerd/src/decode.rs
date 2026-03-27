use serde::Serialize;
use tracing::trace_span;

use cosmwasm_std::{to_json_vec, Event};
// Convert from slay3r types into abci types
use layer_app::{PulsarError, PulsarResult};
use layer_cosmos::{encode_cosmos_response, msg_data_to_proto};

use crate::convert::{consensus_params_to_proto, events_to_proto, validator_updates_to_proto};

pub fn init_response_to_proto(
    response: layer_std::api::InitChainResponse,
) -> tendermint_proto::abci::ResponseInitChain {
    tendermint_proto::abci::ResponseInitChain {
        consensus_params: Some(consensus_params_to_proto(response.consensus_params)),
        validators: validator_updates_to_proto(response.validators),
        app_hash: response.app_hash.into(),
    }
}

pub fn query_response_to_proto(
    response: PulsarResult<layer_std::response::QueryResponse<PulsarError>>,
    height: u64,
) -> tendermint_proto::abci::ResponseQuery {
    match response {
        Ok(response) => {
            let key = match &response {
                layer_std::response::QueryResponse::Raw { key, .. } => key.clone(),
                _ => Vec::new(),
            };
            let value = match encode_cosmos_response(response) {
                Ok(v) => v,
                Err(e) => {
                    return query_error(e, height);
                }
            };
            tendermint_proto::abci::ResponseQuery {
                code: 0,
                log: "".to_string(),
                info: "".to_string(),
                index: 0,
                key: key.into(),
                value: value.into(),
                proof_ops: None,
                height: height.try_into().unwrap(),
                codespace: "".to_string(),
            }
        }
        Err(err) => query_error(err, height),
    }
}

pub(crate) fn query_error(
    err: impl std::error::Error,
    height: u64,
) -> tendermint_proto::abci::ResponseQuery {
    tendermint_proto::abci::ResponseQuery {
        code: 1,
        log: err.to_string(),
        info: "".to_string(),
        index: 0,
        key: Vec::new().into(),
        value: Vec::new().into(),
        proof_ops: None,
        height: height.try_into().unwrap(),
        codespace: "".to_string(),
    }
}

pub fn check_response_to_proto(
    response: layer_std::api::TxResult<PulsarError>,
) -> tendermint_proto::abci::ResponseCheckTx {
    let _span = trace_span!("check_response_to_proto").entered();
    let (gas_wanted, gas_used) = tx_gas_to_proto(response.gas);
    let (code, data, events, log) = tx_result_to_proto(response.result);

    tendermint_proto::abci::ResponseCheckTx {
        code,
        data: data.into(),
        log,
        info: "".to_string(),
        gas_wanted,
        gas_used,
        events,
        codespace: "sdk".to_string(),
    }
}

// This has the same fields as tendermint_proto::abci::ResponseCheckTx but different name,
// so we make helper functions to do the same logic.
pub fn tx_result_to_exec_tx_proto(
    response: layer_std::api::TxResult<PulsarError>,
) -> tendermint_proto::abci::ExecTxResult {
    let _span: tracing::span::EnteredSpan = trace_span!("tx_result_to_exec_tx_proto").entered();
    let (gas_wanted, gas_used) = tx_gas_to_proto(response.gas);
    let (code, data, events, log) = tx_result_to_proto(response.result);

    tendermint_proto::abci::ExecTxResult {
        code,
        data: data.into(),
        log,
        info: "".to_string(),
        gas_wanted,
        gas_used,
        events,
        codespace: "".to_string(),
    }
}

pub fn finalize_response_to_proto(
    response: layer_std::api::FinalizeBlockResponse<PulsarError>,
) -> tendermint_proto::abci::ResponseFinalizeBlock {
    let _span: tracing::span::EnteredSpan = trace_span!("finalize_response_to_proto").entered();
    let tx_results = response
        .tx_results
        .into_iter()
        .map(tx_result_to_exec_tx_proto)
        .collect();
    tendermint_proto::abci::ResponseFinalizeBlock {
        events: events_to_proto(response.events),
        tx_results,
        // TODO: implement these two maps
        validator_updates: validator_updates_to_proto(response.validator_updates),
        consensus_param_updates: response
            .consensus_param_updates
            .map(consensus_params_to_proto),
        app_hash: response.app_hash.into(),
    }
}

fn tx_gas_to_proto(gas: layer_std::api::GasInfo) -> (i64, i64) {
    (
        gas.gas_wanted.try_into().unwrap(),
        gas.gas_used.try_into().unwrap(),
    )
}

#[derive(Serialize, Clone, Debug)]
pub struct LoggedEvents<'a> {
    pub events: &'a [Event],
}

// Yes, this is kind of ridiculous, but we need to encode this like the Cosmos SDK does to be compatible with CosmJS
fn encode_logs(all_events: &[Vec<Event>]) -> String {
    // CosmJS expects [ { events: [event] } ]
    let transform: Vec<_> = all_events
        .iter()
        .map(|events| LoggedEvents { events })
        .collect();
    let encoded = to_json_vec(&transform).unwrap();
    String::from_utf8(encoded).unwrap_or_else(|e| e.to_string())
}

fn tx_result_to_proto(
    result: PulsarResult<layer_std::api::TxResponse>,
) -> (u32, Vec<u8>, Vec<tendermint_proto::abci::Event>, String) {
    let _span: tracing::span::EnteredSpan = trace_span!("tx_result_to_proto").entered();
    match result {
        Ok(resp) => {
            // FIXME: needed for compatibility but slow, review later
            let log = encode_logs(&resp.events);
            let events = resp.events.into_iter().flat_map(events_to_proto).collect();
            let data = msg_data_to_proto(resp.data); // flatten
            (0, data, events, log)
        }
        Err(e) => (1, Vec::new(), Vec::new(), e.to_string()),
    }
}

// TODO: update all the proto for the new prefix
/*
#[cfg(test)]
mod fixtures {
    /// These were pulled from Jaeger fed by CosmJS tests.
    /// That means the input formats are ensured to be compatible with CosmJS and what we can expect.
    use super::*;

    use cosmwasm_std::{coin, Binary, Event};
    use hex_literal::hex;
    use layer_app::PulsarError;
    use layer_std::{
        api::{GasInfo, TxResponse, TxResult},
        must_id,
        response::{
            AccountResponse, AuthQueryResponse, BalanceResponse, BankQueryResponse, QueryResponse,
        },
        BankMsgData, MsgData, PubKey,
    };

    #[test]
    fn encode_account_response() {
        let request = QueryResponse::<PulsarError>::Auth(AuthQueryResponse::Account(
            AccountResponse::External {
                address: must_id("layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug"),
                pubkey: Some(PubKey::Secp256k1(Binary::from(
                    hex!("034f04181eeba35391b858633a765c4a0c189697b40d216354d50890d350c70290")
                        .as_slice(),
                ))),
                sequence: 3,
            },
        ));

        let height = 45;
        let value = hex!("0A9F010A202F636F736D6F732E617574682E763162657461312E426173654163636F756E74127B0A2D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C12460A1F2F636F736D6F732E63727970746F2E736563703235366B312E5075624B657912230A21034F04181EEBA35391B858633A765C4A0C189697B40D216354D50890D350C7029018112003");
        let expected = build_query_success(value.as_slice(), height);
        let proto = query_response_to_proto(Ok(request), height);

        assert_eq!(proto, expected);

        // // response
        // let value = hex!("0A9F010A202F636F736D6F732E617574682E763162657461312E426173654163636F756E74127B0A2D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C12460A1F2F636F736D6F732E63727970746F2E736563703235366B312E5075624B657912230A21034F04181EEBA35391B858633A765C4A0C189697B40D216354D50890D350C7029018112003");
        // let expected = Auth(Account(External { address: pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l, pubkey: Some(Secp256k1(Binary(034f04181eeba35391b858633a765c4a0c189697b40d216354d50890d350c70290))), sequence: 3 }))
    }

    #[test]
    fn encode_balance_response_empty() {
        let request =
            QueryResponse::<PulsarError>::Bank(BankQueryResponse::Balance(BalanceResponse {
                amount: coin(0, "uslay"),
            }));
        let height = 46;
        let value = hex!("0A0B0A067570756C7365120130");
        let expected = build_query_success(value.as_slice(), height);
        let proto = query_response_to_proto(Ok(request), height);
        assert_eq!(proto, expected);
    }

    #[test]
    fn encode_balance_response_full() {
        let request =
            QueryResponse::<PulsarError>::Bank(BankQueryResponse::Balance(BalanceResponse {
                amount: coin(7890, "uslay"),
            }));
        let height = 47;
        let value = hex!("0A0E0A0575736C6179120437383930");
        let expected = build_query_success(value.as_slice(), height);
        let proto = query_response_to_proto(Ok(request), height);
        assert_eq!(proto, expected);
    }

    #[test]
    fn encode_simulate_response() {
        let request = QueryResponse::<PulsarError>::Simulate(TxResult {
            gas: GasInfo {
                gas_used: 5733,
                gas_wanted: 10000000,
            },
            result: Ok(TxResponse {
                data: vec![MsgData::Bank(BankMsgData::Send {})],
                events: vec![vec![Event::new("transfer")
                    .add_attribute("recipient", "layer18jlmr4cta5ecgw96kx40cgvnpaq4ystuy3547a")
                    .add_attribute("sender", "layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug")
                    .add_attribute("amount", "2000000uslay")]],
            }),
        });
        let height = 46;
        let value = hex!("0A080880ADE20410E52C12C3010A200A1E0A1C2F636F736D6F732E62616E6B2E763162657461312E4D736753656E641A9E010A087472616E73666572123C0A09726563697069656E74122D70756C73617231386A6C6D7234637461356563677739366B7834306367766E7061713479737475346E33686E32180112390A0673656E646572122D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C180112190A06616D6F756E74120D323030303030307570756C73651801");
        let expected = build_query_success(value.as_slice(), height);
        let proto = query_response_to_proto(Ok(request), height);
        assert_eq!(proto, expected);

        // // response
        // let value = hex!("0A0B0A067570756C7365120130");
        // let expected = Bank(Balance(BalanceResponse { amount: Coin { denom: "uslay", amount: Uint128(0) } }))

        // let value = hex!("0A0E0A067570756C7365120437383930");
        // let expected = Bank(Balance(BalanceResponse { amount: Coin { denom: "uslay", amount: Uint128(7890) } }))
    }

    fn build_query_success(value: &[u8], height: u64) -> tendermint_proto::abci::ResponseQuery {
        tendermint_proto::abci::ResponseQuery {
            code: 0,
            log: "".to_string(),
            info: "".to_string(),
            index: 0,
            key: vec![].into(),
            value: value.to_vec().into(),
            proof_ops: None,
            height: height as i64,
            codespace: "".to_string(),
        }
    }
}
*/
