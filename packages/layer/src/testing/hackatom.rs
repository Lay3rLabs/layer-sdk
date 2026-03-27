use cosmwasm_std::{coin, coins, to_json_binary};
use layer_std::root::{GovMsg, SystemMsg};
use layer_std::{AccountId, GasError, MsgData, WasmMsg, WasmMsgData};

use crate::genesis::{BankAccount, GenesisState, WasmParams};
use crate::testing::utils::*;
use crate::wasm::WasmError;
use crate::{root_account, PulsarError};

// v1.2.6
const HACKATOM: &[u8] = include_bytes!("../../fixtures/hackatom.wasm");
const DENOM: &str = "uhack";

fn hackatom_genesis(account: &AccountId, gov_key: &AccountId) -> GenesisState {
    GenesisState {
        bank: vec![
            BankAccount {
                address: account.to_string(),
                balance: coins(100_000_000, DENOM),
            },
            BankAccount {
                address: gov_key.to_string(),
                balance: coins(1_000_000, DENOM),
            },
        ],
        wasm: WasmParams {
            gov_account: gov_key.to_string(),
        },
    }
}

struct SetupData {
    app: TestApp,
    signer: PrivateKey,
    #[allow(dead_code)]
    gov_key: PrivateKey,
    code_id: u64,
}

// create and init app
// store the hackatom code
// gov key registers itself as system contract
fn setup(path: &str) -> SetupData {
    let signer = PrivateKey::random();
    let sender = signer.account_id();

    let gov_key = PrivateKey::random();
    let gov_addr = gov_key.account_id();

    let path = prepare_cache(path);
    let mut app = TestApp::new(path);
    let genesis = hackatom_genesis(&sender, &gov_addr);
    app.init(&genesis, "hackatom");

    let msg = WasmMsg::StoreCode {
        sender,
        code: HACKATOM.into(),
    };
    let tx = TxBuilder::new()
        .with_msg(msg)
        .with_signer(&signer, 0)
        .with_fee(1_000_000, coin(50_000, DENOM));
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // parse code_id from first message of first tx
    let events = msg_events(&res[0], 0);
    let code_id = event_value(events, "store_code", "code_id")
        .unwrap()
        .parse()
        .unwrap();

    // ensure proper data response type
    assert!(matches!(
        res.remove(0).result.unwrap().data.remove(0),
        MsgData::Wasm(WasmMsgData::Store { .. })
    ));

    SetupData {
        app,
        signer,
        gov_key,
        code_id,
    }
}

// This will instantiate a new hackatom instance from the given code and given initial balance,
// and return the contract address. Will panic on error
#[track_caller]
fn init_contract(
    app: &mut TestApp,
    code_id: u64,
    signer: &PrivateKey,
    verifier: &AccountId,
    beneficiary: &AccountId,
    funds: u128,
) -> AccountId {
    // find the proper sequence
    let sender = signer.account_id();
    let sequence = app.sequence(&sender).unwrap();

    let init_msg = msgs::InstantiateMsg {
        verifier: verifier.to_string(),
        beneficiary: beneficiary.to_string(),
    };
    let msg = WasmMsg::Instantiate {
        sender: sender.clone(),
        admin: Some(sender),
        code_id,
        msg: to_json_binary(&init_msg).unwrap(),
        funds: coins(funds, DENOM),
        label: "Hackatom Contract".into(),
    };
    let tx = TxBuilder::new().with_msg(msg).with_signer(signer, sequence);
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // parse address from first message of first tx
    let events = msg_events(&res[0], 0);
    let contract =
        AccountId::parse_string(event_value(events, "instantiate", "_contract_address").unwrap())
            .unwrap();

    // ensure it matches the data field
    assert_eq!(
        res.remove(0).result.unwrap().data.remove(0),
        MsgData::Wasm(WasmMsgData::Instantiate {
            contract: contract.clone(),
            data: b"".into()
        })
    );

    contract
}

/// Just like init_contract, but uses instantiate2 and takes a salt
#[track_caller]
fn init_contract2(
    app: &mut TestApp,
    code_id: u64,
    signer: &PrivateKey,
    verifier: &AccountId,
    beneficiary: &AccountId,
    funds: u128,
    salt: &[u8],
) -> AccountId {
    // find the proper sequence
    let sender = signer.account_id();
    let sequence = app.sequence(&sender).unwrap();

    let init_msg = msgs::InstantiateMsg {
        verifier: verifier.to_string(),
        beneficiary: beneficiary.to_string(),
    };
    let msg = WasmMsg::Instantiate2 {
        sender: sender.clone(),
        admin: Some(sender),
        code_id,
        msg: to_json_binary(&init_msg).unwrap(),
        funds: coins(funds, DENOM),
        label: "Hackatom Contract".into(),
        salt: salt.into(),
    };
    let tx = TxBuilder::new().with_msg(msg).with_signer(signer, sequence);
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // parse address from first message of first tx
    let events = msg_events(&res[0], 0);
    let contract =
        AccountId::parse_string(event_value(events, "instantiate", "_contract_address").unwrap())
            .unwrap();

    // ensure it matches the data field
    assert_eq!(
        res.remove(0).result.unwrap().data.remove(0),
        MsgData::Wasm(WasmMsgData::Instantiate {
            contract: contract.clone(),
            data: b"".into()
        })
    );

    contract
}

#[test]
fn basic_hackatom_usage() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/basic-hackatom-usage");

    // other actors
    let verify_key = PrivateKey::random();
    let verifier = verify_key.to_pubkey().account_id().unwrap();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // verify it is set up properly
    let hacks = app.balance(&contract, DENOM).unwrap();
    assert_eq!(hacks.u128(), 10_000_000);
    let hacks = app.balance(&beneficiary, DENOM).unwrap();
    assert_eq!(hacks.u128(), 0);
    let r: msgs::VerifierResponse = app
        .query_wasm(&contract, &msgs::QueryMsg::Verifier {})
        .unwrap();
    assert_eq!(r.verifier, verifier.to_string());

    // verifier can release tokens to the beneficiary
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender: verifier.clone(),
            contract_addr: contract.clone(),
            msg: to_json_binary(&msgs::ExecuteMsg::Release {}).unwrap(),
            funds: vec![],
        })
        .with_signer(&verify_key, 0);
    let mut res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // ensure it matches the data field
    assert!(matches!(
        res.remove(0).result.unwrap().data.remove(0),
        MsgData::Wasm(WasmMsgData::Execute { .. })
    ));

    // verify this was transfered properly
    let hacks = app.balance(&contract, DENOM).unwrap();
    assert_eq!(hacks.u128(), 0);
    let hacks = app.balance(&beneficiary, DENOM).unwrap();
    assert_eq!(hacks.u128(), 10_000_000);
}

#[test]
fn hackatom_with_instantiate_2() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/hackatom-with-instantiate-2");

    // other actors
    let verify_key = PrivateKey::random();
    let verifier = verify_key.to_pubkey().account_id().unwrap();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract2(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
        b"my-salt",
    );

    // verify it is set up properly
    let hacks = app.balance(&contract, DENOM).unwrap();
    assert_eq!(hacks.u128(), 10_000_000);
    let hacks = app.balance(&beneficiary, DENOM).unwrap();
    assert_eq!(hacks.u128(), 0);
    let r: msgs::VerifierResponse = app
        .query_wasm(&contract, &msgs::QueryMsg::Verifier {})
        .unwrap();
    assert_eq!(r.verifier, verifier.to_string());
}

#[test]
fn error_handling_from_api_call() {
    let SetupData {
        app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/error-handling-from-api-call");

    // other actors
    let sender = signer.account_id();
    let verifier = PrivateKey::random().account_id();

    // prepre unsigned tx for simulate
    let init_msg = msgs::InstantiateMsg {
        verifier: verifier.to_string(),
        beneficiary: "some1is3invalid".to_string(),
    };
    let msg = WasmMsg::Instantiate {
        sender: sender.clone(),
        admin: None,
        code_id,
        msg: to_json_binary(&init_msg).unwrap(),
        funds: coins(1_000_000, DENOM),
        label: "Hackatom Contract".into(),
    };
    let tx = TxBuilder::new().with_msg(msg).with_sender(&sender);
    let err = app.simulate(&tx).unwrap().result.unwrap_err();

    match err {
        PulsarError::Wasm(WasmError::Contract(msg)) => {
            assert!(
                msg.starts_with("Generic error: addr_validate errored: Bech32:"),
                "Unexpected error message: {}",
                msg
            );
        }
        x => panic!("Expected ContractError, got: {:?}", x),
    }
}

#[test]
fn check_message_loop() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-message-loop");

    // other actors
    let sender = signer.account_id();
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    let gas_limit = 400_000;
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender,
            contract_addr: contract,
            msg: to_json_binary(&msgs::ExecuteMsg::MessageLoop {}).unwrap(),
            funds: vec![],
        })
        .with_fee(gas_limit, coin(1_000, DENOM))
        .with_signer(&signer, 2);

    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    let res = res.remove(0);
    assert_eq!(res.gas.gas_wanted, gas_limit);
    assert_eq!(res.result, Err(PulsarError::Gas(GasError::OutOfGas)));
    let gas_used = res.gas.gas_used;
    assert!(gas_used >= gas_limit, "gas_used: {}", gas_used);
    assert!(
        gas_used < gas_limit + crate::wasm::keeper::LOAD_WASM_GAS,
        "gas_used: {}",
        gas_used
    );
}

#[test]
fn check_memory_loop() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-memory-loop");

    // other actors
    let sender = signer.account_id();
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // smaller amounts hit out-of-gas (using memory)
    let gas_limit = 100_000;
    let msg = WasmMsg::Execute {
        sender,
        contract_addr: contract,
        msg: to_json_binary(&msgs::ExecuteMsg::MemoryLoop {}).unwrap(),
        funds: vec![],
    };
    let tx = TxBuilder::new()
        .with_msg(msg.clone())
        .with_fee(gas_limit, coin(1_000, DENOM))
        .with_signer(&signer, 2);

    let mut res = app.block(vec![&tx]);
    assert_eq!(res.len(), 1);
    let res = res.remove(0);
    assert_eq!(res.gas.gas_wanted, gas_limit);
    assert_eq!(res.result, Err(PulsarError::Gas(GasError::OutOfGas)));
    let gas_used = res.gas.gas_used;
    assert!(gas_used >= gas_limit, "gas_used: {}", gas_used);
    assert!(
        gas_used < gas_limit + crate::wasm::keeper::LOAD_WASM_GAS,
        "gas_used: {}",
        gas_used
    );

    // larger amounts hit a VM limit before they can use up gas
    let gas_limit = 1_000_000;
    let tx = TxBuilder::new()
        .with_msg(msg)
        .with_fee(gas_limit, coin(1_000, DENOM))
        .with_signer(&signer, 3);
    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    let res = res.remove(0);
    assert_eq!(
        res.result,
        Err(PulsarError::Wasm(WasmError::Vm(
            "Error executing Wasm: Wasmer runtime error: RuntimeError: unreachable".to_string()
        )))
    );
    let gas_used = res.gas.gas_used;
    assert!(
        gas_used < gas_limit,
        "gas_used: {} / {}",
        gas_used,
        gas_limit
    );
}

#[test]
fn check_cpu_loop() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-cpu-loop");

    // other actors
    let sender = signer.account_id();
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    let gas_limit = 831_000;
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender,
            contract_addr: contract,
            msg: to_json_binary(&msgs::ExecuteMsg::CpuLoop {}).unwrap(),
            funds: vec![],
        })
        .with_fee(gas_limit, coin(1_000, DENOM))
        .with_signer(&signer, 2);
    let res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    assert!(res[0].result.is_err());
    assert!(res[0].gas.gas_used >= gas_limit);
    assert!(res[0].gas.gas_used < gas_limit + 10_000);
}

// Note: storage is priced about 5x cheaper than it should compared to cpu usage
// (Free to read from cache is not correctly priced)
#[test]
fn check_storage_loop() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-storage-loop");

    // other actors
    let sender = signer.account_id();
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // execute storage loop with gas limit
    let gas_limit = 376_000;
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender,
            contract_addr: contract,
            msg: to_json_binary(&msgs::ExecuteMsg::StorageLoop {}).unwrap(),
            funds: vec![],
        })
        .with_fee(gas_limit, coin(1_000, DENOM))
        .with_signer(&signer, 2);
    let res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    assert!(res[0].result.is_err());
    assert!(res[0].gas.gas_used >= gas_limit);
    assert!(res[0].gas.gas_used < gas_limit + 10_000);
}

#[test]
fn check_panic_handling() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-panic-handling");

    // other actors
    let sender = signer.account_id();
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Execute {
            sender,
            contract_addr: contract,
            msg: to_json_binary(&msgs::ExecuteMsg::Panic {}).unwrap(),
            funds: vec![],
        })
        .with_fee(200_000, coin(1_000, DENOM))
        .with_signer(&signer, 2);
    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    let r = res.remove(0).result;
    assert_eq!(r, Err(PulsarError::Wasm(WasmError::Vm(
        "Error executing Wasm: Wasmer runtime error: RuntimeError: Aborted: panicked at 'This page intentionally faulted', src/contract.rs:169:5".to_string()
    ))));
}

#[test]
fn check_query_recursion() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-query-recursion");

    // other actors
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // note: gas limit is currently 500k from crate::app::DEFAULT_QUERY_GAS

    // use internal dispatched query and compare to normal query
    let query = &msgs::QueryMsg::Recurse {
        depth: 5,
        work: 100,
    };
    let rec: msgs::RecurseResponse = app.query_wasm(&contract, &query).unwrap();
    assert_eq!(rec.hashed.len(), 32);

    // too much gas will eventually panic (50_000 - 100_000 cycles triggers out of gas)
    // This should run out of gas around 20 depth, instead it stack overflows at 100.
    // (Relies on proper gas accounting in `impl BackendQuerier for VmQuerier`::query_raw

    let query = &msgs::QueryMsg::Recurse {
        depth: 100,
        work: 5000,
    };
    let err = app
        .query_wasm::<_, msgs::RecurseResponse>(&contract, &query)
        .unwrap_err();
    assert_eq!(err, PulsarError::Gas(GasError::OutOfGas));
}

#[test]
fn check_query_balance() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/check-query-balance");

    // other actors
    let sender = signer.account_id();
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // use internal dispatched query and compare to normal query
    let query = &msgs::QueryMsg::OtherBalance {
        address: contract.to_string(),
    };
    let my_bal: cosmwasm_std::AllBalanceResponse = app.query_wasm(&contract, &query).unwrap();
    let expected = app.all_balances(&contract).unwrap();
    assert_eq!(my_bal.amount, expected);

    // use internal dispatched query
    let query = &msgs::QueryMsg::OtherBalance {
        address: sender.to_string(),
    };
    let sender_bal: cosmwasm_std::AllBalanceResponse = app.query_wasm(&contract, &query).unwrap();
    let expected = app.all_balances(&sender).unwrap();
    assert_eq!(sender_bal.amount, expected)
}

#[test]
fn migrate_works() {
    let SetupData {
        mut app,
        signer,
        code_id,
        ..
    } = setup("/tmp/slay3r/migrate-works");

    // other actors
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");
    let sender = signer.account_id();

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // verify it is set up properly
    let r: msgs::VerifierResponse = app
        .query_wasm(&contract, &msgs::QueryMsg::Verifier {})
        .unwrap();
    assert_eq!(r.verifier, verifier.to_string());

    // verifier cannot migrate
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Migrate {
            sender: verifier.clone(),
            contract_addr: contract.clone(),
            new_code_id: code_id,
            msg: to_json_binary(&msgs::MigrateMsg {
                verifier: beneficiary.to_string(),
            })
            .unwrap(),
        })
        .with_signer(&verify_key, 0);
    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    let err = res.remove(0).result.unwrap_err();
    assert_eq!(err, PulsarError::Wasm(WasmError::Unauthorized {}));

    // nothing changed
    let r: msgs::VerifierResponse = app
        .query_wasm(&contract, &msgs::QueryMsg::Verifier {})
        .unwrap();
    assert_eq!(r.verifier, verifier.to_string());

    // admin (sender) can migrate to set self as verifier
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Migrate {
            sender: sender.clone(),
            contract_addr: contract.clone(),
            new_code_id: code_id,
            msg: to_json_binary(&msgs::MigrateMsg {
                verifier: sender.to_string(),
            })
            .unwrap(),
        })
        .with_signer(&signer, 2);
    let res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // it was updated
    let r: msgs::VerifierResponse = app
        .query_wasm(&contract, &msgs::QueryMsg::Verifier {})
        .unwrap();
    assert_eq!(r.verifier, sender.to_string());
}

#[test]
fn sudo_works() {
    let SetupData {
        mut app,
        signer,
        code_id,
        gov_key,
    } = setup("/tmp/slay3r/sudo-works");

    // other actors
    let verify_key = PrivateKey::random();
    let verifier = verify_key.account_id();
    let beneficiary = AccountId::unchecked("beneficiary");
    let sender = signer.account_id();

    // create contract instance with 10_000_000 tokens
    let contract = init_contract(
        &mut app,
        code_id,
        &signer,
        &verifier,
        &beneficiary,
        10_000_000,
    );

    // verify it is set up properly
    assert_eq!(app.balance(&contract, DENOM).unwrap().u128(), 10_000_000);
    assert_eq!(app.balance(&verifier, DENOM).unwrap().u128(), 0);

    // signer cannot do wasm sudo
    let sudo_msg = msgs::SudoMsg::StealFunds {
        recipient: verifier.to_string(),
        amount: coins(7_000_000, DENOM),
    };
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(WasmMsg::Sudo {
            sender: sender.clone(),
            contract_addr: contract.clone(),
            msg: to_json_binary(&sudo_msg).unwrap(),
        })
        .with_signer(&signer, sequence);
    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    let err = res.remove(0).result.unwrap_err();
    assert_eq!(err, PulsarError::Wasm(WasmError::NotRoot));

    // signer cannot call sudo via root contract right now
    let sys_msg = SystemMsg::Sudo {
        contract_addr: contract.to_string(),
        msg: to_json_binary(&sudo_msg).unwrap(),
    };
    let root_msg = WasmMsg::Execute {
        sender: sender.clone(),
        contract_addr: root_account(),
        msg: to_json_binary(&sys_msg).unwrap(),
        funds: vec![],
    };
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(root_msg.clone())
        .with_signer(&signer, sequence);
    let mut res = app.block(&[tx]);
    assert_eq!(res.len(), 1);
    let err = res.remove(0).result.unwrap_err();
    assert_eq!(
        err,
        PulsarError::Wasm(WasmError::Contract("Unauthorized".to_string()))
    );

    // no tokens moved
    assert_eq!(app.balance(&contract, DENOM).unwrap().u128(), 10_000_000);
    assert_eq!(app.balance(&verifier, DENOM).unwrap().u128(), 0);

    // call root as gov, promoting sender, to allow it to directly call as system contract later
    let gov_addr = gov_key.account_id();
    let msg = GovMsg::PromoteContract {
        contract_address: sender.to_string(),
    };
    let msg = WasmMsg::Execute {
        sender: gov_addr,
        contract_addr: root_account(),
        msg: to_json_binary(&msg).unwrap(),
        funds: vec![],
    };
    let tx = TxBuilder::new()
        .with_msg(msg)
        .with_signer(&gov_key, 0)
        .with_fee(1_000_000, coin(0, DENOM));
    let res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // signer can now sudo to eg steal funds
    let sequence = app.sequence(&sender).unwrap();
    let tx = TxBuilder::new()
        .with_msg(root_msg)
        .with_signer(&signer, sequence);
    let res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // tokens were stolen
    assert_eq!(app.balance(&contract, DENOM).unwrap().u128(), 3_000_000);
    assert_eq!(app.balance(&verifier, DENOM).unwrap().u128(), 7_000_000);
}

/// This is copied from https://github.com/CosmWasm/cosmwasm/blob/v1.2.6/contracts/hackatom/src/msg.rs
pub mod msgs {
    use cosmwasm_schema::{cw_serde, QueryResponses};

    use cosmwasm_std::{Binary, Coin};

    #[cw_serde]
    pub struct InstantiateMsg {
        pub verifier: String,
        pub beneficiary: String,
    }

    /// MigrateMsg allows a privileged contract administrator to run
    /// a migration on the contract. In this (demo) case it is just migrating
    /// from one hackatom code to the same code, but taking advantage of the
    /// migration step to set a new validator.
    ///
    /// Note that the contract doesn't enforce permissions here, this is done
    /// by blockchain logic (in the future by blockchain governance)
    #[cw_serde]
    pub struct MigrateMsg {
        pub verifier: String,
    }

    /// SudoMsg is only exposed for internal Cosmos SDK modules to call.
    /// This is showing how we can expose "admin" functionality than can not be called by
    /// external users or contracts, but only trusted (native/Go) code in the blockchain
    #[cw_serde]
    pub enum SudoMsg {
        StealFunds {
            recipient: String,
            amount: Vec<Coin>,
        },
    }

    // failure modes to help test wasmd, based on this comment
    // https://github.com/cosmwasm/wasmd/issues/8#issuecomment-576146751
    #[cw_serde]
    pub enum ExecuteMsg {
        /// Releasing all funds in the contract to the beneficiary. This is the only "proper" action of this demo contract.
        Release {},
        /// Infinite loop to burn cpu cycles (only run when metering is enabled)
        CpuLoop {},
        /// Infinite loop making storage calls (to test when their limit hits)
        StorageLoop {},
        /// Infinite loop reading and writing memory
        MemoryLoop {},
        /// Infinite loop sending message to itself
        MessageLoop {},
        /// Allocate large amounts of memory without consuming much gas
        AllocateLargeMemory { pages: u32 },
        /// Trigger a panic to ensure framework handles gracefully
        Panic {},
        /// Starting with CosmWasm 0.10, some API calls return user errors back to the contract.
        /// This triggers such user errors, ensuring the transaction does not fail in the backend.
        UserErrorsInApiCalls {},
    }

    #[cw_serde]
    #[derive(QueryResponses)]
    pub enum QueryMsg {
        /// returns a human-readable representation of the verifier
        /// use to ensure query path works in integration tests
        #[returns(VerifierResponse)]
        Verifier {},
        /// This returns cosmwasm_std::AllBalanceResponse to demo use of the querier
        #[returns(cosmwasm_std::AllBalanceResponse)]
        OtherBalance { address: String },
        /// Recurse will execute a query into itself up to depth-times and return
        /// Each step of the recursion may perform some extra work to test gas metering
        /// (`work` rounds of sha256 on contract).
        /// Now that we have Env, we can auto-calculate the address to recurse into
        #[returns(RecurseResponse)]
        Recurse { depth: u32, work: u32 },
        /// GetInt returns a hardcoded u32 value
        #[returns(IntResponse)]
        GetInt {},
    }

    #[cw_serde]
    pub struct VerifierResponse {
        pub verifier: String,
    }

    #[cw_serde]
    pub struct RecurseResponse {
        /// hashed is the result of running sha256 "work+1" times on the contract's human address
        pub hashed: Binary,
    }

    #[cw_serde]
    pub struct IntResponse {
        pub int: u32,
    }
}
