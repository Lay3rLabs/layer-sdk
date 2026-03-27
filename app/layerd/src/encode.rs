// Convert from abci types into slay3r types

use layer_cosmos::{parse_cosmos_query, parse_cosmos_tx};
use layer_std::{QueryError, TxError};

use crate::convert::{
    consensus_params_from_proto, timestamp_from_proto, validator_updates_from_proto,
};

pub fn init_request_from_proto(
    request: tendermint_proto::abci::RequestInitChain,
) -> layer_std::api::InitChainRequest {
    layer_std::api::InitChainRequest {
        time: timestamp_from_proto(request.time.unwrap()),
        chain_id: request.chain_id,
        consensus_params: consensus_params_from_proto(request.consensus_params.unwrap()),
        validators: validator_updates_from_proto(request.validators),
        app_state: request.app_state_bytes.to_vec().into(),
        // set the height to 1 if not provided or set to 0 (Go zero means "default" means 1)
        // initial_height: request.initial_height.max(1) as u64,
        initial_height: request.initial_height as u64,
    }
}

pub fn query_request_from_proto(
    request: tendermint_proto::abci::RequestQuery,
    // we need to pass in out-of-bound info for simulate
    chain_id: &str,
) -> Result<layer_std::Query, tendermint_proto::abci::ResponseQuery> {
    if request.height > 0 {
        let err = QueryError::ParseError("Cannot query at historical height".into());
        return Err(crate::decode::query_error(err, 0));
    }
    if request.prove {
        let err = QueryError::ParseError("Cannot prove queries".into());
        return Err(crate::decode::query_error(err, 0));
    }
    parse_cosmos_query(&request.path, request.data, chain_id)
        .map_err(|err| crate::decode::query_error(err, 0))
}

pub fn check_request_from_proto(
    request: tendermint_proto::abci::RequestCheckTx,
    chain_id: &str,
) -> Result<layer_std::Tx, TxError> {
    parse_cosmos_tx(request.tx, chain_id)
}

pub fn finalize_request_from_proto(
    request: tendermint_proto::abci::RequestFinalizeBlock,
    chain_id: &str,
) -> layer_std::api::Block {
    let txs = request
        .txs
        .into_iter()
        // TODO: how to handle failures here... we need responses for them.
        // maybe a re-architecture between splitting the request and response sides
        .map(|tx| parse_cosmos_tx(tx, chain_id).unwrap())
        .collect();
    let last_votes = match request.decided_last_commit {
        None => vec![],
        Some(commit) => commit
            .votes
            .into_iter()
            .filter_map(|vote| {
                vote.validator.map(|v| layer_std::api::Validator {
                    address: v.address.into(),
                    power: v.power.try_into().unwrap(),
                })
            })
            .collect(),
    };
    layer_std::api::Block {
        txs,
        height: request.height.try_into().unwrap(),
        time: timestamp_from_proto(request.time.unwrap()),
        proposer_address: request.proposer_address.into(),
        last_votes,
    }
}

// TODO: update all the proto for the new prefix
/*
#[cfg(test)]
mod fixtures {
    /// These were pulled from Jaeger fed by CosmJS tests.
    /// That means the input formats are ensured to be compatible with CosmJS and what we can expect.
    use super::*;

    use cosmwasm_std::{Binary, Coin, Uint128};
    use hex_literal::hex;
    use layer_std::{
        must_id, AuthQuery, BankMsg, BankQuery, FeeInfo, Msg, PubKey, Query, SignedTx, SigningInfo,
        Tx,
    };

    const CHAIN_ID: &str = "sla-dev-1";

    #[test]
    fn parse_account_query() {
        // query
        let request = tendermint_proto::abci::RequestQuery {
            data: hex!("0A2D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C").as_slice().into(),
            path: "/cosmos.auth.v1beta1.Query/Account".to_string(),
            height: 0,
            prove: false,
        };

        let expected = Query::Auth(AuthQuery::Account {
            address: must_id("pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l"),
        });
        let query = query_request_from_proto(request, CHAIN_ID);

        assert_eq!(query, Ok(expected));

        // // response
        // let value = hex!("0A9F010A202F636F736D6F732E617574682E763162657461312E426173654163636F756E74127B0A2D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C12460A1F2F636F736D6F732E63727970746F2E736563703235366B312E5075624B657912230A21034F04181EEBA35391B858633A765C4A0C189697B40D216354D50890D350C7029018112003");
        // let expected = Auth(Account(External { address: pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l, pubkey: Some(Secp256k1(Binary(034f04181eeba35391b858633a765c4a0c189697b40d216354d50890d350c70290))), sequence: 3 }))
    }

    #[test]
    fn parse_balance_query() {
        // query
        let request = tendermint_proto::abci::RequestQuery {
            data: hex!("0A2D70756C736172316879726C7179796439796A37396334753735687963367568637A65686D6A343378347077686B12067570756C7365").as_slice().into(),
            path: "/cosmos.bank.v1beta1.Query/Balance".to_string(),
            height: 0,
            prove: false,
        };

        let expected = Query::Bank(BankQuery::Balance {
            address: must_id("pulsar1hyrlqyyd9yj79c4u75hyc6uhczehmj43x4pwhk"),
            denom: "uslay".into(),
        });
        let query = query_request_from_proto(request, CHAIN_ID);

        assert_eq!(query, Ok(expected));

        // // response
        // let value = hex!("0A0B0A067570756C7365120130");
        // let expected = Bank(Balance(BalanceResponse { amount: Coin { denom: "uslay", amount: Uint128(0) } }))

        // let value = hex!("0A0E0A067570756C7365120437383930");
        // let expected = Bank(Balance(BalanceResponse { amount: Coin { denom: "uslay", amount: Uint128(7890) } }))
    }

    #[test]
    fn parse_simulate_query() {
        // query
        let raw_tx = hex!("0AAB010A91010A1C2F636F736D6F732E62616E6B2E763162657461312E4D736753656E6412710A2D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C122D70756C73617231386A6C6D7234637461356563677739366B7834306367766E7061713479737475346E33686E321A110A067570756C7365120732303030303030121555736520796F757220706F77657220776973656C7912520A4E0A460A1F2F636F736D6F732E63727970746F2E736563703235366B312E5075624B657912230A21034F04181EEBA35391B858633A765C4A0C189697B40D216354D50890D350C7029012020A00180312001A00");
        let encoded_query = hex!("1284020AAB010A91010A1C2F636F736D6F732E62616E6B2E763162657461312E4D736753656E6412710A2D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C122D70756C73617231386A6C6D7234637461356563677739366B7834306367766E7061713479737475346E33686E321A110A067570756C7365120732303030303030121555736520796F757220706F77657220776973656C7912520A4E0A460A1F2F636F736D6F732E63727970746F2E736563703235366B312E5075624B657912230A21034F04181EEBA35391B858633A765C4A0C189697B40D216354D50890D350C7029012020A00180312001A00");

        let request = tendermint_proto::abci::RequestQuery {
            data: encoded_query.to_vec().into(),
            path: "/cosmos.tx.v1beta1.Service/Simulate".to_string(),
            height: 0,
            prove: false,
        };

        let expected = Query::Simulate(Tx::Signed(SignedTx {
            msgs: vec![Msg::Bank(BankMsg::Send {
                sender: must_id("pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l"),
                recipient: must_id("pulsar18jlmr4cta5ecgw96kx40cgvnpaq4ystu4n3hn2"),
                amount: vec![Coin {
                    denom: "uslay".to_string(),
                    amount: Uint128::new(2000000),
                }],
            })],
            signer: must_id("pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l"),
            signing_info: SigningInfo {
                message_hash: Binary::from(
                    hex!("c6e7ddca8af91d1bedeaeee6ec3e4ed0e4a52544d4573f2e6381393f2f7b1d7d")
                        .as_slice(),
                ),
                sequence: 3,
                pubkey: Some(PubKey::Secp256k1(Binary::from(
                    hex!("034f04181eeba35391b858633a765c4a0c189697b40d216354d50890d350c70290")
                        .as_slice(),
                ))),
                signature: Binary::from(b""),
            },
            fee: FeeInfo {
                fee: None,
                gas_limit: 0,
            },
            timeout_height: None,
            raw_tx: raw_tx.to_vec().into(),
        }));
        let query = query_request_from_proto(request, CHAIN_ID);
        assert_eq!(query, Ok(expected));

        // // response
        // let value = hex!("0A080880ADE20410E52C12C3010A200A1E0A1C2F636F736D6F732E62616E6B2E763162657461312E4D736753656E641A9E010A087472616E73666572123C0A09726563697069656E74122D70756C73617231386A6C6D7234637461356563677739366B7834306367766E7061713479737475346E33686E32180112390A0673656E646572122D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C180112190A06616D6F756E74120D323030303030307570756C73651801");
        // let expected = Simulate(TxResult { gas: GasInfo { gas_used: 5733, gas_wanted: 10000000 }, result: Ok(TxResponse { data: [[]], events: [[Event { ty: "transfer", attributes: [Attribute { key: "recipient", value: "pulsar18jlmr4cta5ecgw96kx40cgvnpaq4ystu4n3hn2" }, Attribute { key: "sender", value: "pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l" }, Attribute { key: "amount", value: "2000000uslay" }] }]] }) })
    }

    /*
    #[test]
    fn parse_finalize_block() {
        // TODO: log all this stuff
        // query
        let request = tendermint_proto::abci::RequestFinalizeBlock {
            txs: todo!(),
            decided_last_commit: todo!(),
            misbehavior: todo!(),
            hash: todo!(),
            height: todo!(),
            time: todo!(),
            next_validators_hash: todo!(),
            proposer_address: todo!(),
        };

        let expected = Query::Simulate(Tx::Signed(SignedTx {
            msgs: vec![Msg::Bank(BankMsg::Send {
                sender: must_id("pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l"),
                recipient: must_id("pulsar18jlmr4cta5ecgw96kx40cgvnpaq4ystu4n3hn2"),
                amount: vec![Coin {
                    denom: "uslay".to_string(),
                    amount: Uint128::new(2000000),
                }],
            })],
            signer: must_id("pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l"),
            signing_info: SigningInfo {
                message_hash: Binary::from(
                    hex!("c228ea379884d11980364c72afe2f670b5bc3968f3c3d7f111b260bf94a8900e")
                        .as_slice(),
                ),
                sequence: 3,
                pubkey: Some(PubKey::Secp256k1(Binary::from(
                    hex!("034f04181eeba35391b858633a765c4a0c189697b40d216354d50890d350c70290")
                        .as_slice(),
                ))),
                signature: Binary::from(b""),
            },
            fee: FeeInfo {
                fee: None,
                gas_limit: 0,
            },
            timeout_height: None,
        }));
        let block = finalize_request_from_proto(request, CHAIN_ID);

        assert_eq!(query, expected);

        // // response
        // let value = hex!("0A080880ADE20410E52C12C3010A200A1E0A1C2F636F736D6F732E62616E6B2E763162657461312E4D736753656E641A9E010A087472616E73666572123C0A09726563697069656E74122D70756C73617231386A6C6D7234637461356563677739366B7834306367766E7061713479737475346E33686E32180112390A0673656E646572122D70756C73617231706B707472653766646B6C366766727A6C65736A6A766878686C63337234676D366B3570336C180112190A06616D6F756E74120D323030303030307570756C73651801");
        // let expected = Simulate(TxResult { gas: GasInfo { gas_used: 5733, gas_wanted: 10000000 }, result: Ok(TxResponse { data: [[]], events: [[Event { ty: "transfer", attributes: [Attribute { key: "recipient", value: "pulsar18jlmr4cta5ecgw96kx40cgvnpaq4ystu4n3hn2" }, Attribute { key: "sender", value: "pulsar1pkptre7fdkl6gfrzlesjjvhxhlc3r4gm6k5p3l" }, Attribute { key: "amount", value: "2000000uslay" }] }]] }) })
    }
    */

    // TODO: add more logging to check_tx as to parsed tx value
    // TODO: add more logging to init_chain as to parsed tx value
}
*/
