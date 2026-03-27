use cosmwasm_std::{coin, coins, to_json_binary, Uint128};
use cw20_base::msg::{InstantiateMsg, QueryMsg};
use layer_std::{AccountId, MsgData, WasmMsg, WasmMsgData};

use crate::genesis::{BankAccount, GenesisState, WasmParams};
use crate::testing::utils::*;
use crate::PulsarResult;

// v1.2.6
const HACKATOM: &[u8] = include_bytes!("../../fixtures/hackatom.wasm");
const CW20: &[u8] = include_bytes!("../../fixtures/cw20_base.wasm");

const DENOM: &str = "unative";
const INIT_BAL_SENDER: u128 = 100_000_000;
const INIT_BAL_GOV: u128 = 1_000_000;

fn base_genesis(account: &AccountId, gov_key: &AccountId) -> GenesisState {
    GenesisState {
        bank: vec![
            BankAccount {
                address: account.to_string(),
                balance: coins(INIT_BAL_SENDER, DENOM),
            },
            BankAccount {
                address: gov_key.to_string(),
                balance: coins(INIT_BAL_GOV, DENOM),
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
    gov_key: PrivateKey,
    code_id: u64,
}

// create and init app
// store the hackatom code
fn setup(path: &str) -> SetupData {
    let signer = PrivateKey::random();
    let sender = signer.account_id();

    let gov_key = PrivateKey::random();

    let path = prepare_cache(path);
    let mut app = TestApp::new(path);
    let genesis = base_genesis(&sender, &gov_key.account_id());
    app.init(&genesis, "hackatom");

    let msg = WasmMsg::StoreCode {
        sender,
        code: CW20.into(),
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

fn query_cw20_balance(app: &TestApp, contract: &AccountId, account: &AccountId) -> Uint128 {
    let msg = QueryMsg::Balance {
        address: account.to_string(),
    };
    let cw20::BalanceResponse { balance } = app.query_wasm(contract, &msg).unwrap();
    balance
}

fn query_cw20_balance_as_native(
    app: &TestApp,
    contract: &AccountId,
    account: &AccountId,
) -> Uint128 {
    let denom = format!("cw20:{}", contract);
    app.balance(account, &denom).unwrap()
}

fn query_cw20_supply(app: &TestApp, contract: &AccountId) -> Uint128 {
    let msg = QueryMsg::TokenInfo {};
    let cw20::TokenInfoResponse { total_supply, .. } = app.query_wasm(contract, &msg).unwrap();
    total_supply
}

fn query_cw20_supply_as_native(app: &TestApp, contract: &AccountId) -> Uint128 {
    let denom = format!("cw20:{}", contract);
    app.supply(&denom).unwrap()
}

fn transfer_cw20_as_native(
    app: &mut TestApp,
    contract: &AccountId,
    from: &PrivateKey,
    to: &AccountId,
    amount: u128,
) -> PulsarResult<()> {
    let denom = format!("cw20:{}", contract);
    app.transfer(from, to, amount, &denom)
}

fn burn_cw20_as_native(
    app: &mut TestApp,
    contract: &AccountId,
    from: &PrivateKey,
    amount: u128,
) -> PulsarResult<()> {
    let denom = format!("cw20:{}", contract);
    app.burn(from, amount, &denom)
}

// This will instantiate a new cw20 instance from the given code, using the provided denom
// It will make signer the minter and provide the given initial balance.
// This returns the contract address. Will panic on error
#[track_caller]
fn init_token(
    app: &mut TestApp,
    code_id: u64,
    symbol: &str,
    signer: &PrivateKey,
    funds: u128,
) -> AccountId {
    // find the proper sequence
    let sender = signer.account_id();
    let sequence = app.sequence(&sender).unwrap();

    let init_msg = InstantiateMsg {
        name: symbol.to_string(),
        symbol: symbol.to_string(),
        decimals: 6,
        initial_balances: vec![cw20::Cw20Coin {
            address: sender.to_string(),
            amount: Uint128::new(funds),
        }],
        mint: Some(cw20::MinterResponse {
            minter: sender.to_string(),
            cap: None,
        }),
        marketing: None,
    };
    let msg = WasmMsg::Instantiate {
        sender: sender.clone(),
        admin: Some(sender),
        code_id,
        msg: to_json_binary(&init_msg).unwrap(),
        funds: vec![],
        label: format!("{} Token", symbol),
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
fn basic_bank_queries() {
    let SetupData {
        mut app,
        signer,
        code_id,
        gov_key,
    } = setup("/tmp/slay3r/basic-bank-queries");
    let sender = signer.account_id();

    // create contract instance with 20_000_000 tokens
    let init_tokens = 20_000_000u128;
    let contract = init_token(&mut app, code_id, "DEMO", &signer, init_tokens);

    // verify gov has native tokens (no gas fees deducted)
    let native = app.balance(&gov_key.account_id(), DENOM).unwrap();
    assert_eq!(native.u128(), INIT_BAL_GOV);

    // verify signer has native tokens (subtracted gas fees paid above)
    let gas_fees = 50_000u128;
    let native = app.balance(&sender, DENOM).unwrap();
    assert_eq!(native.u128(), INIT_BAL_SENDER - gas_fees);

    // verify native supply
    let supply = app.supply(DENOM).unwrap();
    assert_eq!(supply.u128(), INIT_BAL_SENDER + INIT_BAL_GOV);

    // verify signer has cw20 tokens with wasm query
    let cw20 = query_cw20_balance(&app, &contract, &sender);
    assert_eq!(cw20.u128(), init_tokens);
    let cw20 = query_cw20_supply(&app, &contract);
    assert_eq!(cw20.u128(), init_tokens);

    // verify signer has cw20 tokens with bank query
    let cw20 = query_cw20_balance_as_native(&app, &contract, &sender);
    assert_eq!(cw20.u128(), init_tokens);
    let cw20 = query_cw20_supply_as_native(&app, &contract);
    assert_eq!(cw20.u128(), init_tokens);
}

#[test]
fn basic_bank_messages() {
    let SetupData {
        mut app,
        signer,
        code_id,
        gov_key,
    } = setup("/tmp/slay3r/basic-bank-messages");
    let sender = signer.account_id();
    let gov = gov_key.account_id();

    // create contract instance with 20_000_000 tokens
    let init_tokens = 20_000_000u128;
    let contract = init_token(&mut app, code_id, "SEND", &signer, init_tokens);

    // transfer and burn native funds
    let transfer_amount = 3_000_000u128;
    let burn_amount = 1_500_000u128;
    app.transfer(&signer, &gov, transfer_amount, DENOM).unwrap();
    app.burn(&signer, burn_amount, DENOM).unwrap();

    // ensure it works
    let native = app.balance(&gov, DENOM).unwrap();
    assert_eq!(native.u128(), INIT_BAL_GOV + transfer_amount);
    let gas_fees = 50_000u128;
    let native = app.balance(&sender, DENOM).unwrap();
    assert_eq!(
        native.u128(),
        INIT_BAL_SENDER - transfer_amount - burn_amount - gas_fees
    );

    // transfer cw20 funds via bank msg
    transfer_cw20_as_native(&mut app, &contract, &signer, &gov, transfer_amount).unwrap();

    // verify cw20 balances
    let cw20 = query_cw20_supply_as_native(&app, &contract);
    assert_eq!(cw20.u128(), init_tokens);
    let cw20 = query_cw20_balance_as_native(&app, &contract, &sender);
    assert_eq!(cw20.u128(), init_tokens - transfer_amount);
    let cw20 = query_cw20_balance_as_native(&app, &contract, &gov);
    assert_eq!(cw20.u128(), transfer_amount);

    // burn some funds
    burn_cw20_as_native(&mut app, &contract, &signer, burn_amount).unwrap();

    // check new balances
    let cw20 = query_cw20_supply_as_native(&app, &contract);
    assert_eq!(cw20.u128(), init_tokens - burn_amount);
    let cw20 = query_cw20_balance_as_native(&app, &contract, &sender);
    assert_eq!(cw20.u128(), init_tokens - transfer_amount - burn_amount);
}

#[test]
fn contracts_send_receive_cw20_as_native() {
    let SetupData {
        mut app,
        signer,
        code_id,
        gov_key,
    } = setup("/tmp/slay3r/contracts_send_receive_cw20_as_native");
    let sender = signer.account_id();
    let gov = gov_key.account_id();

    // create contract instance with 20_000_000 tokens
    let init_tokens = 20_000_000u128;
    let token = init_token(&mut app, code_id, "SEND", &signer, init_tokens);

    // let's make a hackatom contract storing some of those tokens
    let verify_key = PrivateKey::random();
    let verifier = verify_key.to_pubkey().account_id().unwrap();

    // create contract instance with 8_000_000 tokens
    let escrow_tokens = 8_000_000u128;
    let cw20_denom = format!("cw20:{}", token);
    let escrow = init_hackatom(
        &mut app,
        &signer,
        &verifier,
        &gov,
        escrow_tokens,
        &cw20_denom,
    );

    // verify balances - funds were transfered as part of instantiate (MessageInfo)
    let cw20 = query_cw20_balance_as_native(&app, &token, &sender);
    assert_eq!(cw20.u128(), init_tokens - escrow_tokens);
    let cw20 = query_cw20_balance_as_native(&app, &token, &gov);
    assert_eq!(cw20.u128(), 0);
    let cw20 = query_cw20_balance_as_native(&app, &token, &escrow);
    assert_eq!(cw20.u128(), escrow_tokens);

    // note: this fails as it uses query_all_balances, not the actual cw20 denom it got...
    // https://github.com/CosmWasm/cosmwasm/blob/v1.2.6/contracts/hackatom/src/contract.rs#L92C36-L92C54
    // we actually need a custom contract to properly test this part.

    // // resolve the escrow, this should cause contract to transfer the cw20 tokens out to gov as "native"
    // // note this include contract querying it's own balance as well as returning a BankMsg::Send
    // let tx = TxBuilder::new()
    //     .with_msg(WasmMsg::Execute {
    //         sender: verifier.clone(),
    //         contract_addr: escrow.clone(),
    //         msg: to_json_binary(&super::hackatom::msgs::ExecuteMsg::Release {}).unwrap(),
    //         funds: vec![],
    //     })
    //     .with_signer(&verify_key, 0);
    // let res = app.block(&[tx]);
    // assert_block_success(&res, 1);

    // // verify balances - funds were transfered as part of the return message
    // let cw20 = query_cw20_balance_as_native(&app, &token, &sender);
    // assert_eq!(cw20.u128(), init_tokens - escrow_tokens);
    // let cw20 = query_cw20_balance_as_native(&app, &token, &gov);
    // assert_eq!(cw20.u128(), escrow_tokens); // moved to the recipient
    // let cw20 = query_cw20_balance_as_native(&app, &token, &escrow);
    // assert_eq!(cw20.u128(), 0); // nothing left in escrow
}

// This will instantiate a new hackatom instance from the given code and given initial balance,
// and return the contract address. Will panic on error
#[track_caller]
fn init_hackatom(
    app: &mut TestApp,
    signer: &PrivateKey,
    verifier: &AccountId,
    beneficiary: &AccountId,
    funds: u128,
    denom: &str,
) -> AccountId {
    let sender = signer.account_id();
    let sequence = app.sequence(&sender).unwrap();

    let msg = WasmMsg::StoreCode {
        sender: sender.clone(),
        code: HACKATOM.into(),
    };
    let tx = TxBuilder::new()
        .with_msg(msg)
        .with_signer(signer, sequence)
        .with_fee(1_000_000, coin(50_000, DENOM));
    let res = app.block(&[tx]);
    assert_block_success(&res, 1);

    // parse code_id from first message of first tx
    let events = msg_events(&res[0], 0);
    let code_id = event_value(events, "store_code", "code_id")
        .unwrap()
        .parse()
        .unwrap();

    // find the proper sequence
    let sequence = app.sequence(&sender).unwrap();
    let init_msg = super::hackatom::msgs::InstantiateMsg {
        verifier: verifier.to_string(),
        beneficiary: beneficiary.to_string(),
    };
    let msg = WasmMsg::Instantiate {
        sender: sender.clone(),
        admin: Some(sender),
        code_id,
        msg: to_json_binary(&init_msg).unwrap(),
        funds: coins(funds, denom),
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
