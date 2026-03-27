use bytes::Bytes;
use cosmwasm_schema::serde::{de::DeserializeOwned, Serialize};
use cosmwasm_std::{
    coins, from_json, testing::mock_env, to_json_binary, Binary, Coin, Event, Uint128,
};
use hex_literal::hex;
use itertools::enumerate;
use layer_std::{
    api::{Block, InitChainRequest, TmPubKey, TxResult, ValidatorUpdate},
    response::{
        AccountResponse, AuthQueryResponse, BankQueryResponse, QueryResponse, WasmQueryResponse,
    },
    AccountId, AuthQuery, BankQuery, FeeInfo, Msg, PubKey, Query, SignedTx, SigningInfo, Tx,
    WasmQuery,
};
use layer_storage::MemoryStore;

use crate::{app::App, genesis::GenesisState, AppConfig, PulsarError, PulsarResult, StateMachine};

const NANO_SECOND_PER_BLOCK: u64 = 2_400 * 1_000_000; // 2.4 seconds

pub struct TestApp {
    pub app: App<MemoryStore>,
}

impl TestApp {
    pub fn new(wasm_dir: &str) -> Self {
        let storage = MemoryStore::default();
        let logic = StateMachine::new(&AppConfig::new(wasm_dir));

        // create the app
        let app = App::new(storage, logic);

        Self { app }
    }

    // TODO: use builder pattern for config
    pub fn init(&mut self, genesis: &GenesisState, chain_id: &str) {
        let app_state = to_json_binary(genesis).unwrap();
        let env = mock_env();
        let request = InitChainRequest {
            time: env.block.time,
            chain_id: chain_id.to_string(),
            consensus_params: Default::default(),
            validators: vec![ValidatorUpdate {
                pub_key: TmPubKey::Ed25519(vec![123u8; 32]),
                power: 1_000_000,
            }],
            app_state,
            initial_height: 1,
        };
        self.app.init(request).unwrap();
        // process empty genesis block
        self.block(&[]);
    }

    pub fn height(&self) -> u64 {
        self.app.info().unwrap().height
    }

    pub fn check_tx(&self, tx: &TxBuilder) -> TxResult<PulsarError> {
        self.app.check_tx(tx.build())
    }

    /// Accepts either &[TxBuilder] or vec![&TxBuilder] in case you want to reuse the TxBuilder later (not Cloneable)
    pub fn block<'a>(
        &mut self,
        txs: impl IntoIterator<Item = &'a TxBuilder<'a>> + 'a,
    ) -> Vec<TxResult<PulsarError>> {
        let info = self.app.info().unwrap();

        let block = Block {
            txs: txs.into_iter().map(|tx| tx.build()).collect(),
            height: info.height + 1,
            time: info.time.plus_nanos(NANO_SECOND_PER_BLOCK),
            proposer_address: vec![1u8; 32],
            last_votes: vec![],
        };
        self.app.finalize_block(block).unwrap().tx_results
    }

    pub fn query(&self, query: impl Into<Query>) -> PulsarResult<QueryResponse<PulsarError>> {
        self.app.query(query.into())
    }

    pub fn simulate(&self, tx: &TxBuilder) -> PulsarResult<TxResult<PulsarError>> {
        let res = self.query(Query::Simulate(tx.build()))?;
        match res {
            QueryResponse::Simulate(res) => Ok(res),
            _ => panic!("unexpected response"),
        }
    }

    pub fn all_balances(&self, account: &AccountId) -> PulsarResult<Vec<Coin>> {
        let res = self.query(BankQuery::AllBalances {
            address: account.clone(),
        })?;
        match res {
            QueryResponse::Bank(BankQueryResponse::AllBalances(bal)) => Ok(bal.amount),
            _ => panic!("unexpected response"),
        }
    }

    pub fn balance(&self, account: &AccountId, denom: &str) -> PulsarResult<Uint128> {
        let res = self.query(BankQuery::Balance {
            address: account.clone(),
            denom: denom.to_string(),
        })?;
        match res {
            QueryResponse::Bank(BankQueryResponse::Balance(bal)) => Ok(bal.amount.amount),
            _ => panic!("unexpected response"),
        }
    }

    pub fn supply(&self, denom: &str) -> PulsarResult<Uint128> {
        let res = self.query(BankQuery::Supply {
            denom: denom.to_string(),
        })?;
        match res {
            QueryResponse::Bank(BankQueryResponse::Supply(supply)) => Ok(supply.amount.amount),
            _ => panic!("unexpected response"),
        }
    }

    pub fn sequence(&self, account: &AccountId) -> PulsarResult<u64> {
        let res = self.query(AuthQuery::Account {
            address: account.clone(),
        })?;
        match res {
            QueryResponse::Auth(AuthQueryResponse::Account(acct)) => match acct {
                AccountResponse::External { sequence, .. } => Ok(sequence),
                AccountResponse::Internal { .. } => panic!("unexpected contract account"),
                AccountResponse::Smart { .. } => panic!("unexpected smart account"),
            },
            _ => panic!("unexpected response"),
        }
    }

    pub fn query_wasm<T: Serialize, U: DeserializeOwned>(
        &self,
        contract: &AccountId,
        msg: &T,
    ) -> Result<U, PulsarError> {
        let query = WasmQuery::Smart {
            contract_addr: contract.clone(),
            msg: to_json_binary(msg).unwrap(),
        };
        let res = match self.query(query)? {
            QueryResponse::Wasm(WasmQueryResponse::Smart(result)) => result,
            e => panic!("Unexpected query result: {:?}", e),
        };
        Ok(from_json(res)?)
    }

    pub fn transfer(
        &mut self,
        from: &PrivateKey,
        to: &AccountId,
        amount: u128,
        denom: &str,
    ) -> PulsarResult<()> {
        let sender = from.account_id();
        let seq = self.sequence(&sender).unwrap();
        let tx = TxBuilder::new()
            .with_msg(layer_std::BankMsg::Send {
                sender,
                recipient: to.clone(),
                amount: coins(amount, denom),
            })
            .with_signer(from, seq);
        // submit tx
        let res = self.block(&[tx]);
        // error if any fail
        for r in res {
            r.result?;
        }
        Ok(())
    }

    pub fn burn(&mut self, from: &PrivateKey, amount: u128, denom: &str) -> PulsarResult<()> {
        let sender = from.account_id();
        let seq = self.sequence(&sender).unwrap();
        let tx = TxBuilder::new()
            .with_msg(layer_std::BankMsg::Burn {
                sender,
                amount: coins(amount, denom),
            })
            .with_signer(from, seq);
        // submit tx
        let res = self.block(&[tx]);
        // error if any fail
        for r in res {
            r.result?;
        }
        Ok(())
    }
}

pub fn prepare_cache(path: &'_ str) -> &'_ str {
    let _ = std::fs::remove_dir_all(path);
    std::fs::create_dir_all(path).unwrap();
    path
}

pub fn msg_events(res: &TxResult<PulsarError>, msg: usize) -> &[Event] {
    &res.result.as_ref().unwrap().events[msg]
}

pub fn event_value<'a>(events: &'a [Event], ty: &str, key: &str) -> Option<&'a str> {
    events.iter().find(|a| a.ty == ty).and_then(|evt| {
        evt.attributes
            .iter()
            .find(|a| a.key == key)
            .map(|attr| attr.value.as_str())
    })
}

pub struct TxBuilder<'a> {
    msgs: Vec<Msg>,
    fee: FeeInfo,
    sender: Option<AccountId>,
    signer: Option<(&'a PrivateKey, u64)>,
    invalid_sig: bool,
}

impl<'a> TxBuilder<'a> {
    pub fn new() -> Self {
        Self {
            msgs: vec![],
            fee: FeeInfo {
                gas_limit: 200_000,
                fee: None,
            },
            sender: None,
            signer: None,
            invalid_sig: false,
        }
    }

    pub fn with_msg(mut self, msg: impl Into<Msg>) -> Self {
        self.msgs.push(msg.into());
        self
    }

    pub fn with_fee(mut self, gas_limit: u64, fee: Coin) -> Self {
        self.fee = FeeInfo {
            gas_limit,
            fee: Some(fee),
        };
        self
    }

    // use to simulate unsigned tx, or tx signed by other account
    // if you set a signer, we will determine this automatically
    pub fn with_sender(mut self, sender: &AccountId) -> Self {
        self.sender = Some(sender.clone());
        self
    }

    // use to simulate unsigned tx, or tx signed by other account
    // if you set a signer, we will determine this automatically
    pub fn with_signer(mut self, signer: &'a PrivateKey, sequence: u64) -> Self {
        self.signer = Some((signer, sequence));
        self
    }

    pub fn with_invalid_sig(mut self) -> Self {
        self.invalid_sig = true;
        self
    }

    pub fn build(&self) -> Tx {
        // hardcode this for now..
        let raw_tx = Vec::from(
            hex!("00cafe00deadbeef00cafe009e0b70086ba7c40e91a9d04d31946c10346d79a9").as_slice(),
        );
        let message_hash = Sha256::new_with_prefix(&raw_tx).finalize().to_vec().into();

        let (sequence, pubkey, mut signature) = match self.signer.as_ref() {
            Some((signer, sequence)) => (*sequence, Some(signer.to_pubkey()), signer.sign(&raw_tx)),
            None => (0, None, Binary::from(b"")),
        };
        if self.invalid_sig && !signature.is_empty() {
            // this is a hack to make the signature invalid
            // we just flip the first bit
            let mut swap: Vec<u8> = signature.into();
            swap[0] ^= 0x80;
            signature = swap.into();
        }

        let sender = self
            .sender
            .clone()
            .unwrap_or_else(|| pubkey.as_ref().unwrap().account_id().unwrap());

        Tx::Signed(SignedTx {
            msgs: self.msgs.clone(),
            signer: sender,
            signing_info: SigningInfo {
                message_hash,
                sequence,
                pubkey,
                signature,
            },
            fee: self.fee.clone(),
            timeout_height: None,
            raw_tx: Bytes::from(raw_tx),
        })
    }
}

use k256::ecdsa::{signature::DigestSigner, Signature, SigningKey};
use k256::elliptic_curve::rand_core::OsRng;
use sha2::{Digest, Sha256};

pub struct PrivateKey(SigningKey);

impl PrivateKey {
    #[allow(dead_code)]
    pub fn from_slice(secret: &[u8]) -> Self {
        let sk = SigningKey::from_slice(secret).unwrap();
        Self(sk)
    }

    pub fn random() -> Self {
        let sk = SigningKey::random(&mut OsRng);
        Self(sk)
    }

    pub fn to_pubkey(&self) -> PubKey {
        let pk = self.0.verifying_key().to_encoded_point(true);
        PubKey::secp256k1(Binary::from(pk.as_bytes()))
    }

    pub fn account_id(&self) -> AccountId {
        self.to_pubkey().account_id().unwrap()
    }

    pub fn sign(&self, message: &[u8]) -> Binary {
        let digest = Sha256::new_with_prefix(message);
        let signature: Signature = self.0.sign_digest(digest);
        signature.to_vec().into()
    }
}

#[track_caller]
pub fn assert_block_success(res: &[TxResult<PulsarError>], count: usize) {
    assert_eq!(res.len(), count);

    for (idx, r) in enumerate(res.iter()) {
        match &r.result {
            Ok(_) => {}
            Err(e) => panic!("Tx {} failed: {}", idx, e),
        }
    }
}

#[cfg(test)]
mod test {
    use cosmwasm_std::coin;
    use layer_std::{BankMsg, MsgData};

    use crate::genesis::{BankAccount, WasmParams};

    use super::*;

    const DENOM: &str = "uslay";

    // FIXME: use genesis building pattern?
    fn sample_genesis(account: &AccountId) -> GenesisState {
        let balance = vec![coin(1_000_000, DENOM), coin(2_000_000, "umagic")];
        GenesisState {
            bank: vec![BankAccount {
                address: account.to_string(),
                balance,
            }],
            wasm: WasmParams {
                gov_account: account.to_string(),
            },
        }
    }

    #[test]
    fn can_init_and_query_chain() {
        let account = AccountId::unchecked("foobar");
        let genesis = sample_genesis(&account);

        let wasm_dir = "/tmp/slay3r/can_init_and_query_chain";
        let mut app = TestApp::new(wasm_dir);
        app.init(&genesis, "super-chain");

        let bal = app.balance(&account, DENOM).unwrap();
        assert_eq!(bal.u128(), 1_000_000);

        let bals = app.all_balances(&account).unwrap();
        let expected = vec![coin(2_000_000, "umagic"), coin(1_000_000, DENOM)];
        assert_eq!(bals, expected);

        // copy data and try load_from_store
        let storage = app.app.copy_storage_to_memory();
        let logic = StateMachine::new(&AppConfig::new(wasm_dir));
        let mut app2 = App::new(storage, logic);
        app2.load_from_storage().unwrap();
        let new_app = TestApp { app: app2 };

        // make sure same data here
        let bal = new_app.balance(&account, DENOM).unwrap();
        assert_eq!(bal.u128(), 1_000_000);

        let bals = new_app.all_balances(&account).unwrap();
        let expected = vec![coin(2_000_000, "umagic"), coin(1_000_000, DENOM)];
        assert_eq!(bals, expected);
    }

    #[test]
    fn run_empty_blocks() {
        let account = AccountId::unchecked("foobar");
        let genesis = sample_genesis(&account);

        let mut app = TestApp::new("/tmp/slay3r/run_empty_blocks");
        app.init(&genesis, "super-chain");

        app.block(&[]);
        app.block(&[]);
        app.block(&[]);

        // query works
        let bal = app.balance(&account, DENOM).unwrap();
        assert_eq!(bal.u128(), 1_000_000);

        // height is 4
        assert_eq!(app.height(), 4);
    }

    #[test]
    fn can_check_tx() {
        let pk = PrivateKey::random();
        let signer = pk.to_pubkey();
        let acct = signer.account_id().unwrap();
        let rcpt = AccountId::unchecked("getting paid");

        let mut app = TestApp::new("/tmp/slay3r/can_check_tx");
        let genesis = sample_genesis(&acct);
        app.init(&genesis, "super-chain");

        // build and check tx
        let tx = TxBuilder::new()
            .with_msg(BankMsg::Send {
                sender: acct.clone(),
                recipient: rcpt.clone(),
                amount: vec![coin(123_000, DENOM)],
            })
            .with_signer(&pk, 0);

        // make sure it works
        app.check_tx(&tx).result.unwrap();

        // and bad tx works, as long as we pay fees
        let tx = TxBuilder::new()
            .with_msg(BankMsg::Send {
                sender: rcpt,
                recipient: acct,
                amount: vec![coin(123_000, DENOM)],
            })
            .with_fee(100_000, coin(300_000, DENOM))
            .with_signer(&pk, 0);

        // make sure it succeeds
        app.check_tx(&tx).result.unwrap();
    }

    #[test]
    fn check_tx_failures() {
        let pk = PrivateKey::random();
        let signer = pk.to_pubkey();
        let acct = signer.account_id().unwrap();
        let rcpt = AccountId::unchecked("getting paid");

        let mut app = TestApp::new("/tmp/slay3r/check_tx_failures");
        let genesis = sample_genesis(&acct);
        app.init(&genesis, "super-chain");

        // but too many fees fails
        let tx = TxBuilder::new()
            .with_msg(BankMsg::Send {
                sender: acct.clone(),
                recipient: rcpt.clone(),
                amount: vec![coin(123_000, DENOM)],
            })
            .with_fee(100_000, coin(3_000_000, DENOM))
            .with_signer(&pk, 0);
        app.check_tx(&tx).result.unwrap_err();

        // as does an invalid signature
        let tx = TxBuilder::new()
            .with_msg(BankMsg::Send {
                sender: acct,
                recipient: rcpt,
                amount: vec![coin(123_000, DENOM)],
            })
            .with_invalid_sig()
            .with_signer(&pk, 0);
        app.check_tx(&tx).result.unwrap_err();
    }

    #[test]
    fn can_simulate_tx() {
        let sender = AccountId::unchecked("no private key");
        let rcpt = AccountId::unchecked("getting paid");

        let mut app = TestApp::new("/tmp/slay3r/can_simulate_tx");
        let genesis = sample_genesis(&sender);
        app.init(&genesis, "super-chain");

        // build and simulate tx (even without private key)
        let tx = TxBuilder::new()
            .with_msg(BankMsg::Send {
                sender: sender.clone(),
                recipient: rcpt,
                amount: vec![coin(123_000, DENOM)],
            })
            .with_sender(&sender);

        // make sure it works (even without signer)
        app.simulate(&tx).unwrap();
    }

    #[test]
    fn process_block_with_send() {
        let pk = PrivateKey::random();
        let signer = pk.to_pubkey();
        let acct = signer.account_id().unwrap();
        let rcpt = AccountId::unchecked("getting paid");

        let mut app = TestApp::new("/tmp/slay3r/process_block_with_send");
        let genesis = sample_genesis(&acct);
        app.init(&genesis, "super-chain");

        // query works
        let bal = app.balance(&acct, DENOM).unwrap();
        assert_eq!(bal.u128(), 1_000_000);

        // submit this in a block successfully
        let msg = BankMsg::Send {
            sender: acct.clone(),
            recipient: rcpt.clone(),
            amount: vec![coin(123_000, DENOM)],
        };
        let tx = TxBuilder::new()
            .with_msg(msg.clone())
            .with_fee(100_000, coin(10_000, DENOM))
            .with_signer(&pk, 0);

        let mut res = app.block(&[tx]);
        assert_block_success(&res, 1);
        assert_eq!(
            res.remove(0).result.unwrap().data,
            vec![MsgData::Bank(layer_std::BankMsgData::Send {})]
        );

        // height is 2
        assert_eq!(app.height(), 2);

        // balances updated
        let bal = app.balance(&acct, DENOM).unwrap();
        assert_eq!(bal.u128(), 1_000_000 - 123_000 - 10_000);

        let bal = app.balance(&rcpt, DENOM).unwrap();
        assert_eq!(bal.u128(), 123_000);

        // note, we now reject with sequence 0
        let tx = TxBuilder::new().with_msg(msg.clone()).with_signer(&pk, 0);
        app.check_tx(&tx).result.unwrap_err();

        // and need sequence 1
        let tx = TxBuilder::new().with_msg(msg).with_signer(&pk, 1);
        app.check_tx(&tx).result.unwrap();
    }
}
