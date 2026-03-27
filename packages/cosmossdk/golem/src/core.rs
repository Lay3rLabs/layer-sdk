use layer_app::genesis::{BankAccount, GenesisState, WasmParams};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use abstract_cw_multi_test::AppResponse;
use cosmwasm_std::{to_json_binary, Addr, Binary, BlockInfo, Coin, StdError};

use cw_orch_core::contract::{interface_traits::Uploadable, WasmPath};
use cw_orch_core::environment::{ChainInfo, ChainKind, ChainState, NetworkInfo, TxHandler};

use layer_app::{App, AppConfig, PulsarError, StateMachine};
use layer_std::api::{Block, InitChainRequest, TmPubKey, TxResult, ValidatorUpdate};
use layer_std::{AccountId, BankMsg, FeeInfo, Msg, SigningInfo, Timestamp, WasmMsg};
use layer_storage::MemoryStore;

use crate::{DerivedKey, OrchRegistry};

/// Mock Chain info for the golem. This is used to get the right wasm
pub const MOCK_CHAIN_INFO: ChainInfo = ChainInfo {
    chain_id: "slay3r-orch",
    gas_denom: "uslay",
    gas_price: 0.025,
    grpc_urls: &[],
    lcd_url: None,
    fcd_url: None,
    network_info: NetworkInfo {
        chain_name: "slay3r",
        pub_address_prefix: "slay3r",
        coin_type: 118u32,
    },
    kind: ChainKind::Local,
};

#[derive(Clone)]
pub struct Slay3rGolem {
    pub(crate) config: GolemConfig,
    /// Key used for the operations.
    pub(crate) signer: Rc<DerivedKey>,
    /// Inner mutable state storage for contract addresses and code-ids
    pub(crate) state: Rc<RefCell<OrchRegistry>>,
    // Inner mutable app backend
    pub(crate) app: Rc<RefCell<App<MemoryStore>>>,
}

#[derive(Debug, Clone)]
pub struct GolemConfig {
    /// Block time in milliseconds.
    pub block_time_ms: u64,
    /// Default gas price
    pub gas_price: f64,
    pub gas_denom: String,
    // Chain ID
    pub chain_id: String,
}

impl Default for GolemConfig {
    fn default() -> Self {
        // FIXME: can we configure this along with the MOCK_CHAIN_INFO somehow?
        Self {
            block_time_ms: 2000, // used to update artifical clock when we move forward a block (1 block = 2 second)
            gas_price: MOCK_CHAIN_INFO.gas_price,
            gas_denom: MOCK_CHAIN_INFO.gas_denom.to_string(),
            chain_id: MOCK_CHAIN_INFO.chain_id.to_string(),
        }
    }
}

fn wrap<T>(val: T) -> Rc<RefCell<T>> {
    Rc::new(RefCell::new(val))
}

impl Slay3rGolem {
    pub(crate) fn new(cache_dir: &str, signer: DerivedKey) -> Self {
        let sm = StateMachine::new(&AppConfig::new(cache_dir));
        let app = App::new(MemoryStore::new(), sm);
        let config = GolemConfig::default();
        let output = Self {
            signer: Rc::new(signer),
            state: wrap(OrchRegistry::new(MOCK_CHAIN_INFO.chain_id)),
            app: wrap(app),
            config,
        };
        // FIXME: allow configuring genesis
        output.init();
        output
    }

    fn build_genesis(&self) -> GenesisState {
        let init_balance = vec![Coin {
            denom: self.config.gas_denom.clone(),
            amount: 2_000_000_000u128.into(),
        }];
        // first 20 derived accounts have some tokens
        let accounts = (0..20)
            .map(|i| BankAccount {
                address: self.signer.with_index(i).account().to_string(),
                balance: init_balance.clone(),
            })
            .collect();
        GenesisState {
            bank: accounts,
            wasm: WasmParams {
                gov_account: self.account().to_string(),
            },
        }
    }

    // FIXME: allow configuring genesis
    fn init(&self) {
        let genesis = self.build_genesis();
        let app_state = to_json_binary(&genesis).unwrap();
        let request = InitChainRequest {
            time: Timestamp::from_seconds(seconds_since_epoch()), // current time
            chain_id: self.config.chain_id.clone(),
            consensus_params: Default::default(),
            // TODO: something real (whole mock validator story needs revisiting when we do staking)
            validators: vec![ValidatorUpdate {
                pub_key: TmPubKey::Ed25519(vec![123u8; 32]),
                power: 1_000_000,
            }],
            app_state,
            initial_height: 1,
        };
        self.app.borrow_mut().init(request).unwrap();
    }

    // This clones the daemon but uses a different index for the key
    pub fn with_index(&self, index: u32) -> Self {
        let mut out = self.clone();
        out.signer = self.signer_with_index(index).into();
        out
    }

    // This clones the daemon but uses a different index for the key
    pub fn signer_with_index(&self, index: u32) -> DerivedKey {
        self.signer.with_index(index)
    }

    pub fn account(&self) -> AccountId {
        self.signer.account()
    }

    pub fn block_info(&self) -> BlockInfo {
        let app = self.app.borrow();
        app.info().unwrap().clone()
    }

    pub fn send_tokens(
        &self,
        recipient: &Addr,
        amount: Vec<Coin>,
    ) -> Result<AppResponse, PulsarError> {
        let recipient = AccountId::parse_string(recipient.as_str()).unwrap();
        let msg = BankMsg::Send {
            sender: self.account(),
            recipient,
            amount,
        };
        let tx = self.prepare_tx(msg, None);
        let res = self.run_block(vec![tx])?.pop().unwrap();
        tx_to_app_response(res)
    }

    pub fn run_block(
        &self,
        tx: Vec<layer_std::Tx>,
    ) -> Result<Vec<TxResult<PulsarError>>, PulsarError> {
        let mut app = self.app.borrow_mut();
        let info = app.info().unwrap();
        // TODO: non-empty validators
        let full_block = Block {
            txs: tx,
            height: info.height + 1,
            time: info.time.plus_nanos(self.config.block_time_ms * 1_000_000), // in ms
            proposer_address: vec![],
            last_votes: vec![],
        };
        let res = app.finalize_block(full_block)?;
        Ok(res.tx_results)
    }

    // simple helper for the usual one msg/one tx case
    pub(crate) fn prepare_tx(&self, msg: impl Into<Msg>, gas_limit: Option<u64>) -> layer_std::Tx {
        self.prepare_tx_multi(vec![msg.into()], gas_limit)
    }

    // Use to create a valid "SignedTx" for the given account
    pub(crate) fn prepare_tx_multi(&self, msgs: Vec<Msg>, gas_limit: Option<u64>) -> layer_std::Tx {
        // FIXME: simulate gas fees? (right now hardcoded)
        // Note: block_gas_limit is set by default to 20M
        let gas_limit = gas_limit.unwrap_or(10_000_000u64);
        let gas_amount = (gas_limit as f64 * self.config.gas_price) as u128;
        let fee = FeeInfo {
            fee: Some(Coin {
                denom: self.config.gas_denom.clone(),
                amount: gas_amount.into(),
            }),
            gas_limit,
        };

        // placeholder for signing info
        #[allow(deprecated)]
        let sequence = self.get_sequence(self.sender()).unwrap();
        let signing_info = SigningInfo {
            sequence,
            pubkey: Some(self.signer.pub_key()),
            // these intentionally left blank
            signature: Binary::from(b""),
            message_hash: Binary::from(b""),
        };

        // make tx with no real signing info
        let mut tx = layer_std::SignedTx {
            msgs,
            signer: self.signer.account(),
            fee,
            timeout_height: None,
            signing_info,
            raw_tx: vec![].into(),
        };

        // generate bytes from debug info, then hash and sign
        let tx_bytes = format!("{:?}", tx).into_bytes();
        let message_hash = Sha256::digest(&tx_bytes).to_vec();
        let signature = self.signer.sign_prehash(&message_hash);

        tx.raw_tx = tx_bytes.into();
        tx.signing_info.message_hash = message_hash.into();
        tx.signing_info.signature = signature.into();

        layer_std::Tx::Signed(tx)
    }
}

impl ChainState for Slay3rGolem {
    type Out = Rc<RefCell<OrchRegistry>>;

    fn state(&self) -> Self::Out {
        self.state.clone()
    }
}

impl TxHandler for Slay3rGolem {
    type Response = AppResponse;

    type Error = layer_app::PulsarError;

    type ContractSource = WasmPath;

    type Sender = DerivedKey;

    fn sender(&self) -> Addr {
        self.account().into()
    }

    fn set_sender(&mut self, sender: Self::Sender) {
        self.signer = Rc::new(sender);
    }

    /// Uploads a contract to the chain.
    fn upload<T: Uploadable>(&self, _contract: &T) -> Result<Self::Response, Self::Error> {
        let sender = self.account();
        // load contract wasm
        let file_res = std::fs::read(<T as Uploadable>::wasm(&MOCK_CHAIN_INFO.into()).path());
        let code = file_res
            .map_err(|e| StdError::generic_err(e.to_string()))?
            .into();
        let msg = WasmMsg::StoreCode { sender, code };

        // sign it, run it, convert output
        let tx = self.prepare_tx(msg, None);
        let res = self.run_block(vec![tx])?.pop().unwrap();
        tx_to_app_response(res)
    }

    /// Send a InstantiateMsg to a contract.
    fn instantiate<I: Serialize + Debug>(
        &self,
        code_id: u64,
        init_msg: &I,
        label: Option<&str>,
        admin: Option<&Addr>,
        coins: &[cosmwasm_std::Coin],
    ) -> Result<Self::Response, Self::Error> {
        // construct message format
        let admin = admin
            .map(|a| AccountId::parse_string(a.as_str()))
            .transpose()?;
        let label = label.unwrap_or("default").to_string();
        let msg = to_json_binary(init_msg)?;
        let msg = WasmMsg::Instantiate {
            sender: self.account(),
            admin,
            code_id,
            msg,
            funds: coins.into(),
            label,
        };

        // sign it, run it, convert output
        let tx = self.prepare_tx(msg, None);
        let res = self.run_block(vec![tx])?.pop().unwrap();
        tx_to_app_response(res)
    }

    /// Send a Instantiate2Msg to a contract.
    fn instantiate2<I: Serialize + Debug>(
        &self,
        code_id: u64,
        init_msg: &I,
        label: Option<&str>,
        admin: Option<&Addr>,
        coins: &[cosmwasm_std::Coin],
        salt: Binary,
    ) -> Result<Self::Response, Self::Error> {
        let admin = admin
            .map(|a| AccountId::parse_string(a.as_str()))
            .transpose()?;
        let label = label.unwrap_or("default").to_string();
        let msg = to_json_binary(init_msg)?;
        let msg = WasmMsg::Instantiate2 {
            sender: self.account(),
            admin,
            code_id,
            msg,
            funds: coins.into(),
            label,
            salt,
        };

        // sign it, run it, convert output
        let tx = self.prepare_tx(msg, None);
        let res = self.run_block(vec![tx])?.pop().unwrap();
        tx_to_app_response(res)
    }

    /// Send a ExecMsg to a contract.
    fn execute<E: Serialize + Debug>(
        &self,
        exec_msg: &E,
        coins: &[Coin],
        contract_address: &Addr,
    ) -> Result<Self::Response, Self::Error> {
        let contract_addr = AccountId::parse_string(contract_address.as_str())?;
        let msg = to_json_binary(exec_msg)?;
        let msg = WasmMsg::Execute {
            sender: self.account(),
            contract_addr,
            msg,
            funds: coins.into(),
        };

        // sign it, run it, convert output
        let tx = self.prepare_tx(msg, None);
        let res = self.run_block(vec![tx])?.pop().unwrap();
        tx_to_app_response(res)
    }

    /// Send a MigrateMsg to a contract.
    fn migrate<M: Serialize + Debug>(
        &self,
        migrate_msg: &M,
        new_code_id: u64,
        contract_address: &Addr,
    ) -> Result<Self::Response, Self::Error> {
        let contract_addr = AccountId::parse_string(contract_address.as_str())?;
        let msg = to_json_binary(migrate_msg)?;
        let msg = WasmMsg::Migrate {
            sender: self.account(),
            contract_addr,
            new_code_id,
            msg,
        };

        // sign it, run it, convert output
        let tx = self.prepare_tx(msg, None);
        let res = self.run_block(vec![tx])?.pop().unwrap();
        tx_to_app_response(res)
    }

    /// Clones the chain with a different sender.
    /// Usually used to call a contract as a different sender.
    fn call_as(&self, sender: &<Self as TxHandler>::Sender) -> Self {
        let mut chain = self.clone();
        chain.set_sender(sender.clone());
        chain
    }
}

fn tx_to_app_response(tx: TxResult<PulsarError>) -> Result<AppResponse, PulsarError> {
    // If the tx was a failure, also return error
    let res = tx.result?;
    let events = res.events.into_iter().flatten().collect();
    let data = layer_cosmos::msg_data_to_proto(res.data);

    let output = AppResponse {
        data: Some(data.into()),
        events,
    };
    Ok(output)
}

fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    // use super::*;

    use cosmwasm_std::{coins, Uint128};
    use cw_orch_core::environment::{BankQuerier, DefaultQueriers, TxHandler};

    use crate::Slay3rGolemBuilder;

    // for testing contract uploads... let's see if we can limit so many dependencies...
    use abstract_cw20::{msg::Cw20ExecuteMsgFns, Cw20Coin};
    use abstract_cw20_base::msg::{InstantiateMsg as CW20InstantiateMsg, QueryMsgFns};
    use abstract_cw_plus_interface::cw20_base::Cw20Base;
    use cw_orch_core::contract::interface_traits::{CwOrchInstantiate, CwOrchUpload};

    #[test]
    fn chain_supports_bank() {
        let builder = Slay3rGolemBuilder::new();
        let chain = builder.build();
        assert_eq!(chain.signer.index(), 0);

        // check we initialized balances properly
        let bank_query = chain.bank_querier();
        let balance = bank_query.balance(chain.sender_addr(), None).unwrap();
        assert_eq!(balance, coins(2_000_000_000u128, "uslay"));

        // check we can send a transaction
        let to_send = 123_456_789u128;
        let chain2 = chain.with_index(2);
        let recipient = chain2.sender_addr();
        assert_ne!(chain.sender_addr(), recipient);
        chain
            .send_tokens(&recipient, coins(to_send, "uslay"))
            .unwrap();

        // money arrived
        let balance = bank_query.balance(recipient.clone(), None).unwrap();
        assert_eq!(balance, coins(2_000_000_000u128 + to_send, "uslay"));

        // money sent and gas paid
        let balance = bank_query.balance(chain.sender_addr(), None).unwrap();
        let gas_fees = 250_000u128; // 10M gas * 0.025 uslay/gas (defaults)
        assert_eq!(
            balance,
            coins(2_000_000_000u128 - to_send - gas_fees, "uslay")
        );

        // second send fails until we query sequence
        chain
            .send_tokens(&recipient, coins(to_send, "uslay"))
            .unwrap();
    }

    #[test]
    fn chain_supports_wasm_contract() {
        let chain = Slay3rGolemBuilder::new().build();
        let sender = chain.sender_addr();
        let recipient = chain.with_index(2).sender_addr();
        let init_amount = Uint128::new(55_000_000);

        // why do we need contract id here and not on the task contract?
        let cw20 = Cw20Base::new("my-cw20-base", chain);
        cw20.upload().unwrap();
        let msg = CW20InstantiateMsg {
            name: "slay3r gov token".into(),
            symbol: "SLAY".into(),
            decimals: 6,
            initial_balances: vec![Cw20Coin {
                address: sender.to_string(),
                amount: init_amount,
            }],
            mint: None,
            marketing: None,
        };
        cw20.instantiate(&msg, None, None).unwrap();

        // let's try to query the balance
        let balance = cw20.balance(sender.to_string()).unwrap();
        assert_eq!(balance.balance, init_amount);

        // let's try to transfer the balance
        let amount = Uint128::new(1_000_000);
        cw20.transfer(amount, recipient.to_string()).unwrap();

        // and ensure sender and recipient have properly updated balances
        let sb = cw20.balance(sender.into()).unwrap();
        assert_eq!(sb.balance, init_amount - amount);
        let rb = cw20.balance(recipient.into()).unwrap();
        assert_eq!(rb.balance, amount);
    }
}
