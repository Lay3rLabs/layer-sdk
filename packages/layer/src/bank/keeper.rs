use itertools::Itertools;
use std::collections::HashMap;
use tracing::debug_span;

use cosmwasm_std::{ensure_eq, from_json, to_json_binary, BlockInfo, Coin, Event, Uint128};
use cw_storage_plus::KeyDeserialize;

use layer_std::api::MsgResponse;
use layer_std::response::{
    AllBalanceResponse, BalanceResponse, QueryResponse, SupplyResponse, TotalSupplyResponse,
    WasmQueryResponse,
};
use layer_std::{
    AccountId, AccountIdError, BankMsg, BankMsgData, BankQuery, CoinEncode, GasMeter, QueryError,
    WasmMsg, WasmQuery,
};
use layer_storage::{
    prefixed, prefixed_read, Map, PlusError, PlusResult, ReadonlyStorage, Storage,
};

use crate::bank::BankError;
use crate::error::{PulsarError, PulsarResult};
use crate::genesis::BankAccount;
use crate::sm::StateMachine;

// store supply for each denom
const SUPPLY: Map<&str, Uint128> = Map::new("supply");
// each (user, denom) pair is stored separately for efficient query of one denom
const BALANCES: Map<(&AccountId, &str), Uint128> = Map::new("balances");

pub const NAMESPACE_BANK: &[u8] = b"bank";

pub fn parse_keys(bucket: &str, key: Vec<u8>) -> Vec<String> {
    match bucket {
        "supply" => vec![String::from_vec(key).unwrap()],
        "balances" => {
            let (acct, denom) = <(AccountId, String)>::from_vec(key).unwrap();
            vec![acct.to_string(), denom]
        }
        // FIXME: make robust for production, but this helps debug
        _ => panic!("unknown bucket {}", bucket),
    }
}

enum TypedDenom {
    Native(String),
    Cw20(AccountId),
}

impl TypedDenom {
    const PREFIX: &'static str = "cw20:";

    fn from_denom(denom: &str) -> Result<Self, AccountIdError> {
        match denom.strip_prefix(Self::PREFIX) {
            None => Ok(TypedDenom::Native(denom.into())),
            Some(contract) => {
                let contract = AccountId::parse_string(contract)?;
                Ok(TypedDenom::Cw20(contract))
            }
        }
    }

    // This is pulled out from to_denom for simpler use in some cases
    fn cw20_denom(contract: &AccountId) -> String {
        format!("{}{}", Self::PREFIX, contract)
    }

    #[allow(dead_code)]
    fn to_denom(&self) -> String {
        match self {
            TypedDenom::Native(denom) => denom.clone(),
            TypedDenom::Cw20(contract) => Self::cw20_denom(contract),
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct Bank {}

impl Bank {
    pub fn new() -> Self {
        Bank {}
    }

    /// this is an "admin" function to let us adjust bank accounts in genesis
    /// Should never be called on an initialized account
    pub fn init_balance(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        account: &AccountId,
        amount: Vec<Coin>,
    ) -> PulsarResult<()> {
        let mut bank_storage = prefixed(storage, NAMESPACE_BANK);
        for coin in ValidCoins::new(&amount) {
            let coin = coin?;
            SUPPLY.update::<_, PulsarError>(&mut bank_storage, meter, &coin.denom, |supply| {
                Ok(supply.unwrap_or_default() + coin.amount)
            })?;
            BALANCES.update::<_, PulsarError>(
                &mut bank_storage,
                meter,
                (account, &coin.denom),
                |balance| match balance {
                    None => Ok(coin.amount),
                    Some(_) => {
                        Err(BankError::ReinitializeExistingAccount(account.to_string()).into())
                    }
                },
            )?;
        }
        Ok(())
    }

    fn get_all_balances(
        &self,
        bank_storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        account: &AccountId,
    ) -> PulsarResult<Vec<Coin>> {
        let vals: PlusResult<Vec<_>> = BALANCES
            .prefix(account)
            .range(
                bank_storage,
                meter,
                None,
                None,
                cosmwasm_std::Order::Ascending,
            )?
            .map(|r| {
                let (denom, amount) = r?;
                Ok(Coin { amount, denom })
            })
            .collect();
        Ok(vals?)
    }

    fn get_balance(
        &self,
        bank_storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        account: &AccountId,
        denom: &str,
    ) -> PulsarResult<Coin> {
        let amount = BALANCES
            .may_load(bank_storage, meter, (account, denom))?
            .unwrap_or_default();
        Ok(Coin {
            amount,
            denom: denom.to_string(),
        })
    }

    fn get_cw20_balance(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        account: &AccountId,
        contract_addr: AccountId,
    ) -> PulsarResult<Coin> {
        let denom = TypedDenom::cw20_denom(&contract_addr);
        let request = WasmQuery::Smart {
            contract_addr,
            msg: to_json_binary(&cw20::Cw20QueryMsg::Balance {
                address: account.to_string(),
            })?,
        };
        let res = sm.wasm.query(storage, meter, block, sm, request)?;
        match res {
            QueryResponse::Wasm(WasmQueryResponse::Smart(res)) => {
                let cw20::BalanceResponse { balance } = from_json(res)?;
                Ok(Coin {
                    amount: balance,
                    denom,
                })
            }
            _ => Err(QueryError::ParseError("unexpected response".into()).into()),
        }
    }

    fn get_supply(
        &self,
        bank_storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        denom: &str,
    ) -> PulsarResult<Uint128> {
        let val = SUPPLY.may_load(bank_storage, meter, denom)?;
        Ok(val.unwrap_or_default())
    }

    fn get_cw20_supply(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        contract_addr: AccountId,
    ) -> PulsarResult<Uint128> {
        let request = WasmQuery::Smart {
            contract_addr,
            msg: to_json_binary(&cw20::Cw20QueryMsg::TokenInfo {})?,
        };
        let res = sm.wasm.query(storage, meter, block, sm, request)?;
        match res {
            QueryResponse::Wasm(WasmQueryResponse::Smart(res)) => {
                let cw20::TokenInfoResponse { total_supply, .. } = from_json(res)?;
                Ok(total_supply)
            }
            _ => Err(QueryError::ParseError("unexpected response".into()).into()),
        }
    }

    fn send_native(
        &self,
        bank_storage: &mut dyn Storage,
        meter: &GasMeter,
        from_address: &AccountId,
        to_address: &AccountId,
        coin: &Coin,
    ) -> PulsarResult<()> {
        // remove from old account account balance
        BALANCES
            .update::<_, PlusError>(
                bank_storage,
                meter,
                (from_address, &coin.denom),
                |balance| Ok(balance.unwrap_or_default().checked_sub(coin.amount)?),
            )
            .map_err(|_| BankError::InsufficientFunds(from_address.to_string()))?;
        // add to new account balance
        BALANCES.update::<_, PulsarError>(
            bank_storage,
            meter,
            (to_address, &coin.denom),
            |balance| Ok(balance.unwrap_or_default() + coin.amount),
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn send_cw20(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        from_address: &AccountId,
        to_address: &AccountId,
        contract_addr: AccountId,
        amount: Uint128,
    ) -> PulsarResult<()> {
        let msg = to_json_binary(&cw20::Cw20ExecuteMsg::Transfer {
            recipient: to_address.to_string(),
            amount,
        })?;
        let msg = WasmMsg::Execute {
            sender: from_address.clone(),
            contract_addr,
            msg,
            funds: vec![],
        };
        sm.wasm
            .process_msg_no_submsg(storage, meter, block, sm, from_address, msg)?;
        Ok(())
    }

    fn mint(
        &self,
        bank_storage: &mut dyn Storage,
        meter: &GasMeter,
        to_address: AccountId,
        amount: Vec<Coin>,
    ) -> PulsarResult<()> {
        for coin in ValidCoins::new(&amount) {
            let coin = coin?;
            // add to the supply
            SUPPLY.update::<_, PulsarError>(bank_storage, meter, &coin.denom, |supply| {
                Ok(supply.unwrap_or_default() + coin.amount)
            })?;
            // and to the account balance
            BALANCES.update::<_, PulsarError>(
                bank_storage,
                meter,
                (&to_address, &coin.denom),
                |balance| Ok(balance.unwrap_or_default() + coin.amount),
            )?;
        }
        Ok(())
    }

    fn burn_native(
        &self,
        bank_storage: &mut dyn Storage,
        meter: &GasMeter,
        from_address: &AccountId,
        coin: &Coin,
    ) -> PulsarResult<()> {
        // remove from the supply
        SUPPLY.update::<_, PlusError>(bank_storage, meter, &coin.denom, |supply| {
            Ok(supply.unwrap_or_default().checked_sub(coin.amount)?)
        })?;
        // and to the account balance
        BALANCES
            .update::<_, PlusError>(
                bank_storage,
                meter,
                (from_address, &coin.denom),
                |balance| Ok(balance.unwrap_or_default().checked_sub(coin.amount)?),
            )
            .map_err(|_| BankError::InsufficientFunds(from_address.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn burn_cw20(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        from_address: &AccountId,
        contract_addr: AccountId,
        amount: Uint128,
    ) -> PulsarResult<()> {
        let msg = to_json_binary(&cw20::Cw20ExecuteMsg::Burn { amount })?;
        let msg = WasmMsg::Execute {
            sender: from_address.clone(),
            contract_addr,
            msg,
            funds: vec![],
        };
        sm.wasm
            .process_msg_no_submsg(storage, meter, block, sm, from_address, msg)?;
        Ok(())
    }
}

impl Bank {
    // helper to move funds when called from another module
    #[allow(clippy::too_many_arguments)]
    pub fn transfer(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        from_address: AccountId,
        to_address: AccountId,
        amount: Vec<Coin>,
    ) -> PulsarResult<()> {
        let _span =
            debug_span!("transfer", %from_address, %to_address, amount = %CoinEncode(&amount))
                .entered();
        for coin in ValidCoins::new(&amount) {
            let coin = coin?;
            match TypedDenom::from_denom(&coin.denom)? {
                TypedDenom::Native(_) => {
                    let mut bank_storage = prefixed(storage, NAMESPACE_BANK);
                    self.send_native(&mut bank_storage, meter, &from_address, &to_address, coin)?;
                }
                TypedDenom::Cw20(contract) => {
                    self.send_cw20(
                        storage,
                        meter,
                        block,
                        sm,
                        &from_address,
                        &to_address,
                        contract,
                        coin.amount,
                    )?;
                }
            }
        }
        Ok(())
    }

    // helper to move funds when called from another module
    pub fn burn(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        from_address: AccountId,
        amount: Vec<Coin>,
    ) -> PulsarResult<()> {
        let _span = debug_span!("burn", %from_address, amount = %CoinEncode(&amount)).entered();
        for coin in ValidCoins::new(&amount) {
            let coin = coin?;
            match TypedDenom::from_denom(&coin.denom)? {
                TypedDenom::Native(_) => {
                    let mut bank_storage = prefixed(storage, NAMESPACE_BANK);
                    self.burn_native(&mut bank_storage, meter, &from_address, coin)?;
                }
                TypedDenom::Cw20(contract) => {
                    self.burn_cw20(
                        storage,
                        meter,
                        block,
                        sm,
                        &from_address,
                        contract,
                        coin.amount,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn init(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        _block: &BlockInfo,
        accounts: Vec<BankAccount>,
        _sm: &StateMachine,
    ) -> PulsarResult<()> {
        let mut bank_storage = prefixed(storage, NAMESPACE_BANK);
        for account in accounts {
            let address = AccountId::parse_string(&account.address)?;
            self.mint(&mut bank_storage, meter, address, account.balance)?;
        }
        Ok(())
    }

    pub fn process_msg(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        signer: &AccountId,
        msg: BankMsg,
    ) -> PulsarResult<MsgResponse> {
        match msg {
            BankMsg::Send {
                sender,
                recipient,
                amount,
            } => {
                ensure_eq!(signer, &sender, BankError::Unauthorized);
                let events = vec![Event::new("transfer")
                    .add_attribute("recipient", &recipient)
                    .add_attribute("sender", &sender)
                    .add_attribute("amount", coins_to_string(&amount))];
                self.transfer(storage, meter, block, sm, sender, recipient, amount)?;
                Ok(MsgResponse::new(events, BankMsgData::Send {}))
            }
            BankMsg::Burn { sender, amount } => {
                ensure_eq!(signer, &sender, BankError::Unauthorized);
                let events = vec![Event::new("burn")
                    .add_attribute("sender", &sender)
                    .add_attribute("amount", coins_to_string(&amount))];
                self.burn(storage, meter, block, sm, sender, amount)?;
                Ok(MsgResponse::new(events, BankMsgData::Burn {}))
            }
        }
    }

    pub fn query(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        _block: &BlockInfo,
        _sm: &StateMachine,
        request: BankQuery,
    ) -> PulsarResult<QueryResponse<PulsarError>> {
        let bank_storage = prefixed_read(storage, NAMESPACE_BANK);
        match request {
            BankQuery::AllBalances { address } => {
                let amount = self.get_all_balances(&bank_storage, meter, &address)?;
                let res = AllBalanceResponse { amount };
                Ok(res.into())
            }
            BankQuery::Balance { address, denom } => {
                let amount = match TypedDenom::from_denom(&denom)? {
                    TypedDenom::Native(denom) => {
                        self.get_balance(&bank_storage, meter, &address, &denom)?
                    }
                    TypedDenom::Cw20(contract) => {
                        self.get_cw20_balance(storage, meter, _block, _sm, &address, contract)?
                    }
                };
                let res = BalanceResponse { amount };
                Ok(res.into())
            }
            BankQuery::TotalSupply {} => {
                let amounts = SUPPLY
                    .range(
                        &bank_storage,
                        meter,
                        None,
                        None,
                        cosmwasm_std::Order::Ascending,
                    )?
                    .map(|r| {
                        let (denom, amount) = r?;
                        Ok(Coin {
                            amount,
                            denom: denom.to_string(),
                        })
                    })
                    .collect::<PlusResult<Vec<_>>>()?;
                let res = TotalSupplyResponse { amounts };
                Ok(res.into())
            }
            BankQuery::Supply { denom } => {
                let amount = match TypedDenom::from_denom(&denom)? {
                    TypedDenom::Native(denom) => self.get_supply(&bank_storage, meter, &denom)?,
                    TypedDenom::Cw20(contract) => {
                        self.get_cw20_supply(storage, meter, _block, _sm, contract)?
                    }
                };
                let res = SupplyResponse {
                    amount: Coin { denom, amount },
                };
                Ok(res.into())
            }
        }
    }
}

struct ValidCoins<'a> {
    coins: std::slice::Iter<'a, Coin>,
    seen: HashMap<&'a str, bool>,
}

impl<'a> ValidCoins<'a> {
    fn new(coins: &'a [Coin]) -> Self {
        ValidCoins {
            coins: coins.iter(),
            seen: HashMap::new(),
        }
    }
}

impl<'a> Iterator for ValidCoins<'a> {
    type Item = Result<&'a Coin, BankError>;

    /// Rules:
    /// If zero amount, filter out.
    /// If denom seen before, return error
    /// Otherwise, return coin
    /// At end of iterator, if no non-zero amount seen, return error
    fn next(&mut self) -> Option<Self::Item> {
        let val = self.coins.next();
        match val {
            None => {
                if self.seen.is_empty() {
                    Some(Err(BankError::NoEmptyTransfer))
                } else {
                    None
                }
            }
            // filter out zero amounts via recursion
            Some(c) if c.amount.is_zero() => self.next(),
            Some(c) => {
                if self.seen.contains_key(&c.denom.as_str()) {
                    Some(Err(BankError::DuplicateDenom(c.denom.clone())))
                } else {
                    self.seen.insert(&c.denom, true);
                    Some(Ok(c))
                }
            }
        }
    }
}

fn coins_to_string(coins: &[Coin]) -> String {
    coins
        .iter()
        .map(|c| format!("{}{}", c.amount, c.denom))
        .join(",")
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::error::PulsarError;
    use crate::AppConfig;
    use cosmwasm_std::testing::mock_env;
    use cosmwasm_std::{coin, coins, StdError};
    use layer_std::response::BankQueryResponse;
    use layer_storage::{MemoryStore, PersistentStorage, Storage};

    fn query_balance(bank: &Bank, store: &dyn Storage, rcpt: &AccountId) -> Vec<Coin> {
        let req = BankQuery::AllBalances {
            address: rcpt.clone(),
        };
        let block = mock_env().block;
        let meter = GasMeter::new(500_000);
        let sm = StateMachine::new(&AppConfig::new("/tmp/slay3r/query_balance"));

        let resp = bank
            .query(store.as_ref(), &meter, &block, &sm, req)
            .unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::AllBalances(AllBalanceResponse { amount })) => {
                amount
            }
            _ => panic!("unexpected return"),
        }
    }

    fn query_supply(bank: &Bank, store: &dyn Storage, denom: &str) -> Uint128 {
        let req = BankQuery::Supply {
            denom: denom.into(),
        };
        let block = mock_env().block;
        let meter = GasMeter::new(500_000);
        let sm = StateMachine::new(&AppConfig::new("/tmp/slay3r/query_supply"));

        let resp = bank
            .query(store.as_ref(), &meter, &block, &sm, req)
            .unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::Supply(SupplyResponse { amount })) => {
                amount.amount
            }
            _ => panic!("unexpected return"),
        }
    }

    #[test]
    fn get_set_balance() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let block = mock_env().block;
        let sm = StateMachine::new(&AppConfig::new("/tmp/slay3r/get_set_balance"));
        let meter = GasMeter::new(500_000);

        let owner = AccountId::unchecked("owner");
        let rcpt = AccountId::unchecked("receiver");
        let init_funds = vec![coin(100, "eth"), coin(20, "btc")];
        let norm = vec![coin(20, "btc"), coin(100, "eth")];

        // set money
        let bank = Bank::new();
        bank.init_balance(&mut store, &meter, &owner, init_funds)
            .unwrap();
        let bank_storage = prefixed_read(&store, NAMESPACE_BANK);

        // get balance work
        let rich = bank
            .get_all_balances(&bank_storage, &meter, &owner)
            .unwrap();
        assert_eq!(rich, norm);
        let poor = bank.get_all_balances(&bank_storage, &meter, &rcpt).unwrap();
        assert_eq!(poor, vec![]);

        // proper queries work
        let req = BankQuery::AllBalances {
            address: owner.clone(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::AllBalances(AllBalanceResponse { amount })) => {
                assert_eq!(amount, norm)
            }
            _ => panic!("unexpected return"),
        }

        let req = BankQuery::AllBalances {
            address: rcpt.clone(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::AllBalances(AllBalanceResponse { amount })) => {
                assert_eq!(amount, vec![])
            }
            _ => panic!("unexpected return"),
        }

        let req = BankQuery::Balance {
            address: owner.clone(),
            denom: "eth".into(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::Balance(BalanceResponse { amount })) => {
                assert_eq!(amount, coin(100, "eth"))
            }
            _ => panic!("unexpected return"),
        }

        let req = BankQuery::Balance {
            address: owner,
            denom: "foobar".into(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::Balance(BalanceResponse { amount })) => {
                assert_eq!(amount, coin(0, "foobar"))
            }
            _ => panic!("unexpected return"),
        }

        let req = BankQuery::Balance {
            address: rcpt,
            denom: "eth".into(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::Balance(BalanceResponse { amount })) => {
                assert_eq!(amount, coin(0, "eth"))
            }
            _ => panic!("unexpected return"),
        }

        let req = BankQuery::Supply {
            denom: "eth".into(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::Supply(SupplyResponse { amount })) => {
                assert_eq!(amount, coin(100, "eth"))
            }
            _ => panic!("unexpected return"),
        }

        let req = BankQuery::Supply {
            denom: "foobar".into(),
        };
        let resp = bank.query(&store, &meter, &block, &sm, req).unwrap();
        match resp {
            QueryResponse::Bank(BankQueryResponse::Supply(SupplyResponse { amount })) => {
                assert_eq!(amount, coin(0, "foobar"))
            }
            _ => panic!("unexpected return"),
        }
    }

    #[test]
    fn send_coins() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let meter = GasMeter::new(1_000_000);
        let block = mock_env().block;
        let sm = StateMachine::new(&AppConfig::new("/tmp/slay3r/send_coins"));

        let owner = AccountId::unchecked("owner");
        let rcpt = AccountId::unchecked("receiver");
        let init_funds = vec![coin(20, "btc"), coin(100, "eth")];
        let rcpt_funds = vec![coin(5, "btc")];

        // set money
        let bank = Bank::new();
        bank.init_balance(&mut store, &meter, &owner, init_funds)
            .unwrap();
        bank.init_balance(&mut store, &meter, &rcpt, rcpt_funds)
            .unwrap();

        // send both tokens
        let to_send = vec![coin(30, "eth"), coin(5, "btc")];
        let msg = BankMsg::Send {
            sender: owner.clone(),
            recipient: rcpt.clone(),
            amount: to_send,
        };
        bank.process_msg(&mut store, &meter, &block, &sm, &owner, msg.clone())
            .unwrap();
        let rich = query_balance(&bank, &store, &owner);
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);
        let poor = query_balance(&bank, &store, &rcpt);
        assert_eq!(vec![coin(10, "btc"), coin(30, "eth")], poor);

        // cannot send from someone else's account
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &rcpt, msg)
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::Unauthorized));

        // cannot send too much
        let msg = BankMsg::Send {
            sender: owner.clone(),
            recipient: rcpt.clone(),
            amount: coins(20, "btc"),
        };
        bank.process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap_err();

        let rich = query_balance(&bank, &store, &owner);
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);
    }

    #[test]
    fn burn_coins() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let block = mock_env().block;
        let meter = GasMeter::new(1_000_000);
        let sm = StateMachine::new(&AppConfig::new("/tmp/slay3r/burn_coins"));

        let owner = AccountId::unchecked("owner");
        let rcpt = AccountId::unchecked("recipient");
        let init_funds = vec![coin(20, "btc"), coin(100, "eth")];

        // set money
        let bank = Bank::new();
        bank.init_balance(&mut store, &meter, &owner, init_funds)
            .unwrap();

        // burn both tokens
        let to_burn = vec![coin(30, "eth"), coin(5, "btc")];
        let msg = BankMsg::Burn {
            sender: owner.clone(),
            amount: to_burn,
        };
        bank.process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap();
        let rich = query_balance(&bank, &store, &owner);
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);

        // cannot burn too much
        let msg = BankMsg::Burn {
            sender: owner.clone(),
            amount: coins(20, "btc"),
        };
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap_err();
        assert!(matches!(err, PulsarError::Std(StdError::Overflow { .. })));

        let rich = query_balance(&bank, &store, &owner);
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);

        // cannot burn from empty account
        let msg = BankMsg::Burn {
            sender: rcpt.clone(),
            amount: coins(1, "btc"),
        };
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &rcpt, msg)
            .unwrap_err();
        assert_eq!(
            err,
            PulsarError::Bank(BankError::InsufficientFunds(rcpt.to_string()))
        );
    }

    #[test]
    fn supply_tracked_properly() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let meter = GasMeter::new(1_000_000);

        let owner = AccountId::unchecked("owner");
        let rcpt = AccountId::unchecked("receiver");
        let init_funds = vec![coin(20, "btc"), coin(100, "eth")];
        let rcpt_funds = vec![coin(5, "btc")];

        // set money
        let bank = Bank::new();
        bank.init_balance(&mut store, &meter, &owner, init_funds)
            .unwrap();
        bank.init_balance(&mut store, &meter, &rcpt, rcpt_funds)
            .unwrap();

        // check original supply
        let btc = query_supply(&bank, &store, "btc");
        assert_eq!(btc.u128(), 25);
        let eth = query_supply(&bank, &store, "eth");
        assert_eq!(eth.u128(), 100);

        // send some tokens will not modify supply
        // TODO: use transfer here, but needs block and sm which we didn't set up
        let to_send = [coin(30, "eth"), coin(5, "btc")];
        {
            let mut bank_store = prefixed(&mut store, NAMESPACE_BANK);
            bank.send_native(&mut bank_store, &meter, &owner, &rcpt, &to_send[0])
                .unwrap();
            bank.send_native(&mut bank_store, &meter, &owner, &rcpt, &to_send[1])
                .unwrap();
        }
        // check balance properly updated (already covered above)
        let rich = query_balance(&bank, &store, &owner);
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);
        // check supply didn't change
        let btc = query_supply(&bank, &store, "btc");
        assert_eq!(btc.u128(), 25);
        let eth = query_supply(&bank, &store, "eth");
        assert_eq!(eth.u128(), 100);

        // burn tokens will reduce supply
        {
            let mut bank_store = prefixed(&mut store, NAMESPACE_BANK);
            bank.burn_native(&mut bank_store, &meter, &owner, &coin(7, "btc"))
                .unwrap();
        }
        let rich = query_balance(&bank, &store, &owner);
        assert_eq!(vec![coin(8, "btc"), coin(70, "eth")], rich);
        let btc = query_supply(&bank, &store, "btc");
        assert_eq!(btc.u128(), 18);

        // mint tokens will increase supply
        let mut bstore = prefixed(&mut store, NAMESPACE_BANK);
        bank.mint(&mut bstore, &meter, rcpt.clone(), coins(77, "eth"))
            .unwrap();
        let poor = query_balance(&bank, &store, &rcpt);
        assert_eq!(vec![coin(10, "btc"), coin(107, "eth")], poor);
        let eth = query_supply(&bank, &store, "eth");
        assert_eq!(eth.u128(), 177);
    }

    #[test]
    fn fail_on_zero_values() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let meter = GasMeter::new(1_000_000);
        let block = mock_env().block;
        let sm = StateMachine::new(&AppConfig::new("/tmp/slay3r/fail_on_zero_values"));

        let owner = AccountId::unchecked("owner");
        let rcpt = AccountId::unchecked("recipient");
        let init_funds = vec![coin(5000, "atom"), coin(100, "eth")];

        // set money
        let bank = Bank::new();
        bank.init_balance(&mut store, &meter, &owner, init_funds)
            .unwrap();

        // can send normal amounts
        let msg = BankMsg::Send {
            sender: owner.clone(),
            recipient: rcpt.clone(),
            amount: coins(100, "atom"),
        };
        bank.process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap();

        // fails send on no coins
        let msg = BankMsg::Send {
            sender: owner.clone(),
            recipient: rcpt.clone(),
            amount: vec![],
        };
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::NoEmptyTransfer));

        // fails send on 0 coins
        let msg = BankMsg::Send {
            sender: owner.clone(),
            recipient: rcpt.clone(),
            amount: coins(0, "atom"),
        };
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::NoEmptyTransfer));

        // fails burn on no coins
        let msg = BankMsg::Burn {
            sender: owner.clone(),
            amount: vec![],
        };
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::NoEmptyTransfer));

        // fails burn on 0 coins
        let msg = BankMsg::Burn {
            sender: owner.clone(),
            amount: coins(0, "atom"),
        };
        let err = bank
            .process_msg(&mut store, &meter, &block, &sm, &owner, msg)
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::NoEmptyTransfer));

        // can mint
        let mut bank_storage = prefixed(&mut store, NAMESPACE_BANK);
        bank.mint(&mut bank_storage, &meter, rcpt.clone(), coins(4321, "atom"))
            .unwrap();

        // mint fails with 0 tokens
        let err = bank
            .mint(&mut bank_storage, &meter, rcpt.clone(), coins(0, "atom"))
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::NoEmptyTransfer));

        // mint fails with no tokens
        let err = bank
            .mint(&mut bank_storage, &meter, rcpt, vec![])
            .unwrap_err();
        assert_eq!(err, PulsarError::Bank(BankError::NoEmptyTransfer));
    }

    #[test]
    fn valid_coins() {
        // empty transfer
        let a: Result<Vec<_>, BankError> = ValidCoins::new(&[]).collect();
        assert_eq!(a.unwrap_err(), BankError::NoEmptyTransfer);

        // only 0 is also empty
        let only_zeros = &[coin(0, "ucosm"), coin(0, "uwasm")];
        let a: Result<Vec<_>, BankError> = ValidCoins::new(only_zeros).collect();
        assert_eq!(a.unwrap_err(), BankError::NoEmptyTransfer);

        // all valid coins are passed through (unsorted)
        let all_valid: &[Coin] = &[coin(234, "uwasm"), coin(17, "ucosm")];
        let a: Result<Vec<_>, BankError> = ValidCoins::new(all_valid).collect();
        // trying to compare Vec<&Coin> with &[Coin] makes use manually transform
        assert_eq!(a.unwrap(), vec![&all_valid[0], &all_valid[1]]);

        // 0 values are filtered out and don't count towards duplicate
        let all_valid: &[Coin] = &[coin(0, "uwasm"), coin(876, "uwasm")];
        let a: Result<Vec<_>, BankError> = ValidCoins::new(all_valid).collect();
        // trying to compare Vec<&Coin> with &[Coin] makes use manually transform
        assert_eq!(a.unwrap(), vec![&all_valid[1]]);

        // two non-zero entries with same denom is duplicate error
        let all_valid: &[Coin] = &[coin(876, "uwasm"), coin(876, "uwasm")];
        let a: Result<Vec<_>, BankError> = ValidCoins::new(all_valid).collect();
        assert_eq!(
            a.unwrap_err(),
            BankError::DuplicateDenom("uwasm".to_string())
        );
    }
}
