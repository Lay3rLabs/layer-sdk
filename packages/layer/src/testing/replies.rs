use cosmwasm_std::{coin, coins, to_json_binary, Event};
use layer_std::api::TxResponse;
use layer_std::{AccountId, GasError, MsgData, WasmMsg, WasmMsgData};

use crate::genesis::{BankAccount, GenesisState, WasmParams};
use crate::testing::utils::*;

const ECHO: &[u8] = include_bytes!("../../fixtures/tc_echo.wasm");
const CALLER: &[u8] = include_bytes!("../../fixtures/tc_caller.wasm");
const DENOM: &str = "urepl";

fn genesis(account: &AccountId) -> GenesisState {
    GenesisState {
        bank: vec![BankAccount {
            address: account.to_string(),
            balance: coins(100_000_000, DENOM),
        }],
        wasm: WasmParams {
            gov_account: account.to_string(),
        },
    }
}

struct SetupData {
    app: TestApp,
    signer: PrivateKey,
    echo_id: u64,
    caller_id: u64,
}

// create and init app
// store the hackatom code
fn setup(path: &str) -> SetupData {
    let signer = PrivateKey::random();
    let sender = signer.account_id();

    let path = prepare_cache(path);
    let mut app = TestApp::new(path);
    let genesis = genesis(&sender);
    app.init(&genesis, "replies-1");

    let msg = WasmMsg::StoreCode {
        sender: sender.clone(),
        code: ECHO.into(),
    };
    let tx = TxBuilder::new()
        .with_msg(msg)
        .with_signer(&signer, 0)
        .with_fee(2_000_000, coin(50_000, DENOM));
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);
    // parse code_id from response data
    let MsgData::Wasm(WasmMsgData::Store {
        code_id: echo_id, ..
    }) = res.remove(0).result.unwrap().data.remove(0)
    else {
        panic!("unexpected response data");
    };

    let msg = WasmMsg::StoreCode {
        sender,
        code: CALLER.into(),
    };
    let tx = TxBuilder::new()
        .with_msg(msg)
        .with_signer(&signer, 1)
        .with_fee(2_000_000, coin(50_000, DENOM));
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);
    // parse code_id from response data
    let MsgData::Wasm(WasmMsgData::Store {
        code_id: caller_id, ..
    }) = res.remove(0).result.unwrap().data.remove(0)
    else {
        panic!("unexpected response data");
    };

    SetupData {
        app,
        signer,
        echo_id,
        caller_id,
    }
}

struct InitData {
    contract: AccountId,
    echo_contract: AccountId,
    res: TxResponse,
}

// This will instantiate a new hackatom instance from the given code and given initial balance,
// and return the contract address. Will panic on error
#[track_caller]
fn init_contract(
    app: &mut TestApp,
    signer: &PrivateKey,
    caller_id: u64,
    init_msg: &tc_caller::InstantiateMsg,
) -> InitData {
    // find the proper sequence
    let sender = signer.account_id();
    let sequence = app.sequence(&sender).unwrap();

    let msg = WasmMsg::Instantiate {
        sender: sender.clone(),
        admin: Some(sender),
        code_id: caller_id,
        msg: to_json_binary(init_msg).unwrap(),
        funds: vec![],
        label: "Caller Contract".into(),
    };
    let tx = TxBuilder::new().with_msg(msg).with_signer(signer, sequence);
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);
    let res = res.remove(0);

    // parse address from first message of first tx
    let events = msg_events(&res, 0);
    let contract =
        AccountId::parse_string(event_value(events, "instantiate", "_contract_address").unwrap())
            .unwrap();

    // skip first two and from the remainder, we can find the echo instantiation
    let echo_contract = AccountId::parse_string(
        event_value(&events[2..], "instantiate", "_contract_address").unwrap(),
    )
    .unwrap();

    InitData {
        contract,
        echo_contract,
        res: res.result.unwrap(),
    }
}

#[test]
fn basic_init_callback_and_catching_errors() {
    let SetupData {
        mut app,
        signer,
        caller_id,
        echo_id,
    } = setup("/tmp/slay3r/basic_init_callback_and_catching_errors");
    let sender = signer.account_id();

    let subcall = tc_caller::CallInfo {
        reply_on: cosmwasm_std::ReplyOn::Always,
        override_data: true,
        gas_limit: None,
    };

    // simple init message with no problems
    let init_msg = tc_caller::InstantiateMsg {
        code_id: echo_id,
        msg: tc_echo::InstantiateMsg::Echo(tc_echo::EchoMsg {
            data: Some(b"from echo".into()),
            attrs: vec![],
            events: vec![Event::new("test-one").add_attribute("some", "more")],
        }),
        subcall: subcall.clone(),
    };

    // create contract instance with 10_000_000 tokens
    let InitData {
        contract,
        echo_contract,
        res,
    } = init_contract(&mut app, &signer, caller_id, &init_msg);

    // Let's check the data field overridden in the contract
    let data = match &res.data[0] {
        MsgData::Wasm(WasmMsgData::Instantiate { data, .. }) => data.as_slice(),
        _ => panic!("unexpected message type"),
    };
    assert_eq!(data, b"from echo");

    // Check the order of events
    assert_eq!(res.events.len(), 1);
    // instantiate and reply added by the framework. wasm-instantiate comes from caller, and wasm-test-one from echo
    let names = res.events[0]
        .iter()
        .map(|e| e.ty.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "instantiate",
            "wasm-instantiate",
            "instantiate",
            "wasm-test-one",
            "reply"
        ]
    );

    // catch a failure from the contract
    let exec_msg = tc_caller::ExecuteMsg {
        msg: tc_echo::ExecuteMsg::Fail {
            msg: "Oh, no!".to_string(),
        },
        subcall,
    };
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender,
            contract_addr: contract.clone(),
            msg: to_json_binary(&exec_msg).unwrap(),
            funds: vec![],
        })
        .with_signer(&signer, sequence);
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);
    let res = res.remove(0).result.unwrap();

    // Let's check the data field as not overridden by the contract
    let data = match &res.data[0] {
        MsgData::Wasm(WasmMsgData::Execute { data }) => data.as_slice(),
        _ => panic!("unexpected message type"),
    };
    assert_eq!(data, b"exec");

    // Check the events - nothing emitted from the echo contract,
    assert_eq!(res.events.len(), 1);
    let events = &res.events[0];
    let names = events.iter().map(|e| e.ty.as_str()).collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["execute", "wasm-execute", "reply", "wasm-reply"]
    );
    // but caller returns error in wasm-reply message
    assert_eq!(events[3].attributes.len(), 2);
    let contract_attr = &events[3].attributes[0];
    assert_eq!(contract_attr.key.as_str(), "_contract_address");
    assert_eq!(contract_attr.value.as_str(), &contract.to_string());
    let error_attr = &events[3].attributes[1];
    assert_eq!(error_attr.key.as_str(), "error");
    assert_eq!(error_attr.value.as_str(), "Contract Error: Oh, no!");

    // query counters on the contracts
    let tc_caller::CounterResponse { calls, replies } = app
        .query_wasm(&contract, &tc_caller::QueryMsg::Counter {})
        .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(replies, 2);

    // query the counts on echo contract - state write on error should have been reverted, only one recorded
    let tc_echo::CounterResponse { count } = app
        .query_wasm(&echo_contract, &tc_echo::QueryMsg::Counter {})
        .unwrap();
    assert_eq!(count, 1);

    // TODO: exec success with non-overriding data field to verify handling
}

// TODO: check gas handling (limit higher than the actual gas used, limit lower - catch error, limit lower - don't catch error)
#[test]
fn submsg_gas_limits() {
    let SetupData {
        mut app,
        signer,
        caller_id,
        echo_id,
    } = setup("/tmp/slay3r/submsg_gas_limits");
    let sender = signer.account_id();

    // simple init message with no problems
    let init_msg = tc_caller::InstantiateMsg {
        code_id: echo_id,
        msg: tc_echo::InstantiateMsg::Echo(tc_echo::EchoMsg {
            data: None,
            attrs: vec![],
            events: vec![],
        }),
        subcall: tc_caller::CallInfo {
            reply_on: cosmwasm_std::ReplyOn::Always,
            override_data: true,
            gas_limit: None,
        },
    };

    // create contract instance
    let InitData {
        contract,
        echo_contract,
        res: _,
    } = init_contract(&mut app, &signer, caller_id, &init_msg);

    let echo = tc_echo::ExecuteMsg::Echo(tc_echo::EchoMsg {
        data: None,
        attrs: vec![],
        events: vec![Event::new("success")],
    });

    // Execute success with high gas limit
    let exec_msg = tc_caller::ExecuteMsg {
        msg: echo.clone(),
        subcall: tc_caller::CallInfo {
            reply_on: cosmwasm_std::ReplyOn::Never,
            override_data: false,
            gas_limit: Some(100_000),
        },
    };
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender: sender.clone(),
            contract_addr: contract.clone(),
            msg: to_json_binary(&exec_msg).unwrap(),
            funds: vec![],
        })
        .with_signer(&signer, sequence);
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // make sure we got expected event
    let res = res.remove(0).result.unwrap();
    assert_eq!(res.events.len(), 1);
    let names = res.events[0]
        .iter()
        .map(|e| e.ty.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["execute", "wasm-execute", "execute", "wasm-success"]
    );

    // Abort tx when low gas limit and no reply block
    let exec_msg = tc_caller::ExecuteMsg {
        msg: echo.clone(),
        subcall: tc_caller::CallInfo {
            reply_on: cosmwasm_std::ReplyOn::Never,
            override_data: false,
            gas_limit: Some(64_000),
        },
    };
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender: sender.clone(),
            contract_addr: contract.clone(),
            msg: to_json_binary(&exec_msg).unwrap(),
            funds: vec![],
        })
        .with_signer(&signer, sequence);
    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    println!("Gas: {:?}", res[0].gas);
    let err = res.remove(0).result.unwrap_err();
    assert_eq!(err, GasError::OutOfGas.into());

    // Catching reply when low gas limit
    let exec_msg = tc_caller::ExecuteMsg {
        msg: echo,
        subcall: tc_caller::CallInfo {
            reply_on: cosmwasm_std::ReplyOn::Error,
            override_data: false,
            gas_limit: Some(64_000), // 64k is big enough to be inside contract execution, not just setup
        },
    };
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender,
            contract_addr: contract.clone(),
            msg: to_json_binary(&exec_msg).unwrap(),
            funds: vec![],
        })
        .with_signer(&signer, sequence);
    let mut res = app.block(&[tx]);
    let res = res.remove(0).result.unwrap();
    assert_eq!(res.events.len(), 1);
    let names = res.events[0]
        .iter()
        .map(|e| e.ty.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["execute", "wasm-execute", "reply", "wasm-reply"]
    );
    // check the error message we get
    let error_attr = &res.events[0][3].attributes[1];
    assert_eq!(error_attr.key.as_str(), "error");
    assert_eq!(error_attr.value.as_str(), "Out of gas");

    // query counters on the contracts (init, success, caught error)
    let tc_caller::CounterResponse { calls, replies } = app
        .query_wasm(&contract, &tc_caller::QueryMsg::Counter {})
        .unwrap();
    assert_eq!(calls, 3);
    assert_eq!(replies, 2);

    // query the counts on echo contract - only init, success recorder
    let tc_echo::CounterResponse { count } = app
        .query_wasm(&echo_contract, &tc_echo::QueryMsg::Counter {})
        .unwrap();
    assert_eq!(count, 2);
}
