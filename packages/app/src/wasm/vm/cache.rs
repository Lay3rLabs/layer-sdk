use std::{collections::HashSet, fmt};

use cosmwasm_std::{Binary, Env, MessageInfo, Reply, Response};
use cosmwasm_vm::{
    call_execute, call_instantiate, call_migrate, call_query, call_reply, call_sudo,
    AnalysisReport, Cache, CacheOptions, Checksum, InstanceOptions, Size, VmError,
};
use layer_std::{AccountId, GasMeter};
use layer_storage::{AppMeter, ReadonlyStorage, ScratchTx, Storage, WeakSubTx};

use crate::{wasm::keeper::contract_storage, StateMachine};

use super::backend::{danger_will_robinson, out_of_gas, VmApi, VmQuerier, VmStore};

const DEFAULT_CACHE_MB: usize = 500;
const DEFAULT_INSTANCE_MB: usize = 32;
const CAPABILITIES: &[&str] = &[
    "iterator",
    "staking",
    "stargate",
    "cosmwasm_1_1",
    "cosmwasm_1_2",
    "cosmwasm_1_3",
    "cosmwasm_1_4",
    // 2.0:
    // "cosmwasm_2_0",
];
const PRINT_DEBUG: bool = false;

// TODO: what is this really?
// Changed by 1000 in 2.0 upgrade: https://github.com/CosmWasm/cosmwasm/pull/1884
// 2.0: down to 150_000
const SDK_TO_WASMER_GAS_FACTOR: u64 = 150_000_000;

pub fn sdk_gas_to_wasmer(gas: u64) -> u64 {
    gas.saturating_mul(SDK_TO_WASMER_GAS_FACTOR)
}

pub fn wasmer_gas_to_sdk(gas: u64) -> u64 {
    gas / SDK_TO_WASMER_GAS_FACTOR
}

fn capabilities() -> HashSet<String> {
    CAPABILITIES.iter().map(|s| s.to_string()).collect()
}

pub struct VmCache {
    cache: Cache<VmApi, VmStore, VmQuerier>,
    print_debug: bool,
}

impl fmt::Debug for VmCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmCache")
            .field("print_debug", &self.print_debug)
            .finish()
    }
}

impl VmCache {
    // TODO: make more args?
    pub fn init(cache_dir: &str) -> Self {
        let cache_options = CacheOptions::new(
            cache_dir,
            capabilities(),
            Size::mebi(DEFAULT_CACHE_MB),
            Size::mebi(DEFAULT_INSTANCE_MB),
        );
        let cache = unsafe { Cache::new(cache_options).unwrap() };
        VmCache {
            cache,
            print_debug: PRINT_DEBUG,
        }
    }

    pub fn store_code(&self, wasm: &[u8]) -> Result<(Checksum, AnalysisReport), VmError> {
        let checksum = self.cache.save_wasm(wasm)?;
        let analysis = self.cache.analyze(&checksum)?;
        Ok((checksum, analysis))
    }

    #[allow(dead_code)]
    pub fn load_code(&self, checksum: &Checksum) -> Result<Vec<u8>, VmError> {
        self.cache.load_wasm(checksum)
    }

    pub fn pin(&self, checksum: &Checksum) -> Result<(), VmError> {
        self.cache.pin(checksum)
    }

    pub fn unpin(&self, checksum: &Checksum) -> Result<(), VmError> {
        self.cache.unpin(checksum)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn instantiate(
        &self,
        checksum: &Checksum,
        env: &Env,
        info: &MessageInfo,
        msg: &[u8],
        global_storage: &mut dyn Storage,
        contract: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> (
        Result<Result<Response<super::CustomMsg>, String>, VmError>,
        u64,
    ) {
        let gas_limit = sdk_gas_to_wasmer(meter.remaining());
        let options = InstanceOptions {
            gas_limit,
            print_debug: self.print_debug,
        };

        // Create WeakSubTx that only holds readable access, so we can query underlying storage as contract is working
        let query = global_storage.as_ref();
        let mut wrap = WeakSubTx::new(query);
        let mut working = contract_storage(&mut wrap, contract);

        // This is where we fake all the lifetimes....
        let backend = unsafe { danger_will_robinson(sm, &mut working, query, meter, &env.block) };

        let mut instance = match self.cache.get_instance(checksum, backend, options) {
            Ok(i) => i,
            // No gas used yet
            Err(e) => return (Err(e), 0),
        };

        // execute the contract and get gas_used
        instance.set_storage_readonly(false);
        let result = call_instantiate(&mut instance, env, info, msg);
        let result = result.map(|x| x.into_result());
        let gas_used = wasmer_gas_to_sdk(instance.create_gas_report().used_internally);

        // commit or abort the open WeakSubTx
        match &result {
            Ok(Ok(_)) => {
                let ops = wrap.prepare();
                if let Err(e) = ops.commit(global_storage, meter).map_err(out_of_gas) {
                    return (Err(e.into()), gas_used);
                }
            }
            _ => working.abort(),
        };
        let _ = instance.recycle();

        (result, gas_used)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        checksum: &Checksum,
        env: &Env,
        info: &MessageInfo,
        msg: &[u8],
        global_storage: &mut dyn Storage,
        contract: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> (
        Result<Result<Response<super::CustomMsg>, String>, VmError>,
        u64,
    ) {
        let gas_limit = sdk_gas_to_wasmer(meter.remaining());
        let options = InstanceOptions {
            gas_limit,
            print_debug: self.print_debug,
        };

        // Create WeakSubTx that only holds readable access, so we can query underlying storage as contract is working
        let query = global_storage.as_ref();
        let mut wrap = WeakSubTx::new(query);
        let mut working = contract_storage(&mut wrap, contract);

        // This is where we fake all the lifetimes....
        let backend = unsafe { danger_will_robinson(sm, &mut working, query, meter, &env.block) };

        let mut instance = match self.cache.get_instance(checksum, backend, options) {
            Ok(i) => i,
            // No gas used yet
            Err(e) => return (Err(e), 0),
        };

        // execute the contract and get gas_used
        instance.set_storage_readonly(false);
        let result = call_execute(&mut instance, env, info, msg);
        let result = result.map(|x| x.into_result());
        let gas_used = wasmer_gas_to_sdk(instance.create_gas_report().used_internally);

        // commit or abort the open WeakSubTx
        match &result {
            Ok(Ok(_)) => {
                let ops = wrap.prepare();
                if let Err(e) = ops.commit(global_storage, meter).map_err(out_of_gas) {
                    return (Err(e.into()), gas_used);
                }
            }
            _ => working.abort(),
        };
        let _ = instance.recycle();

        (result, gas_used)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn migrate(
        &self,
        checksum: &Checksum,
        env: &Env,
        msg: &[u8],
        global_storage: &mut dyn Storage,
        contract: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> (
        Result<Result<Response<super::CustomMsg>, String>, VmError>,
        u64,
    ) {
        let gas_limit = sdk_gas_to_wasmer(meter.remaining());
        let options = InstanceOptions {
            gas_limit,
            print_debug: self.print_debug,
        };

        // Create WeakSubTx that only holds readable access, so we can query underlying storage as contract is working
        let query = global_storage.as_ref();
        let mut wrap = WeakSubTx::new(query);
        let mut working = contract_storage(&mut wrap, contract);

        // This is where we fake all the lifetimes....
        let backend = unsafe { danger_will_robinson(sm, &mut working, query, meter, &env.block) };

        let mut instance = match self.cache.get_instance(checksum, backend, options) {
            Ok(i) => i,
            // No gas used yet
            Err(e) => return (Err(e), 0),
        };

        // execute the contract and get gas_used
        instance.set_storage_readonly(false);
        let result = call_migrate(&mut instance, env, msg);
        let result = result.map(|x| x.into_result());
        let gas_used = wasmer_gas_to_sdk(instance.create_gas_report().used_internally);

        // commit or abort the open WeakSubTx
        match &result {
            Ok(Ok(_)) => {
                let ops = wrap.prepare();
                if let Err(e) = ops.commit(global_storage, meter).map_err(out_of_gas) {
                    return (Err(e.into()), gas_used);
                }
            }
            _ => working.abort(),
        };
        let _ = instance.recycle();

        (result, gas_used)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sudo(
        &self,
        checksum: &Checksum,
        env: &Env,
        msg: &[u8],
        global_storage: &mut dyn Storage,
        contract: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> (
        Result<Result<Response<super::CustomMsg>, String>, VmError>,
        u64,
    ) {
        let gas_limit = sdk_gas_to_wasmer(meter.remaining());
        let options = InstanceOptions {
            gas_limit,
            print_debug: self.print_debug,
        };

        // Create WeakSubTx that only holds readable access, so we can query underlying storage as contract is working
        let query = global_storage.as_ref();
        let mut wrap = WeakSubTx::new(query);
        let mut working = contract_storage(&mut wrap, contract);

        // This is where we fake all the lifetimes....
        let backend = unsafe { danger_will_robinson(sm, &mut working, query, meter, &env.block) };

        let mut instance = match self.cache.get_instance(checksum, backend, options) {
            Ok(i) => i,
            // No gas used yet
            Err(e) => return (Err(e), 0),
        };

        // execute the contract and get gas_used
        instance.set_storage_readonly(false);
        let result = call_sudo(&mut instance, env, msg);
        let result = result.map(|x| x.into_result());
        let gas_used = wasmer_gas_to_sdk(instance.create_gas_report().used_internally);

        // commit or abort the open WeakSubTx
        match &result {
            Ok(Ok(_)) => {
                let ops = wrap.prepare();
                if let Err(e) = ops.commit(global_storage, meter).map_err(out_of_gas) {
                    return (Err(e.into()), gas_used);
                }
            }
            _ => working.abort(),
        };
        let _ = instance.recycle();

        (result, gas_used)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reply(
        &self,
        checksum: &Checksum,
        env: &Env,
        reply: &Reply,
        global_storage: &mut dyn Storage,
        contract: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> (
        Result<Result<Response<super::CustomMsg>, String>, VmError>,
        u64,
    ) {
        let gas_limit = sdk_gas_to_wasmer(meter.remaining());
        let options = InstanceOptions {
            gas_limit,
            print_debug: self.print_debug,
        };

        // Create WeakSubTx that only holds readable access, so we can query underlying storage as contract is working
        let query = global_storage.as_ref();
        let mut wrap = WeakSubTx::new(query);
        let mut working = contract_storage(&mut wrap, contract);

        // This is where we fake all the lifetimes....
        let backend = unsafe { danger_will_robinson(sm, &mut working, query, meter, &env.block) };

        let mut instance = match self.cache.get_instance(checksum, backend, options) {
            Ok(i) => i,
            // No gas used yet
            Err(e) => return (Err(e), 0),
        };

        // execute the contract and get gas_used
        instance.set_storage_readonly(false);
        let result = call_reply(&mut instance, env, reply);
        let result = result.map(|x| x.into_result());
        let gas_used = wasmer_gas_to_sdk(instance.create_gas_report().used_internally);

        // commit or abort the open WeakSubTx
        match &result {
            Ok(Ok(_)) => {
                let ops = wrap.prepare();
                if let Err(e) = ops.commit(global_storage, meter).map_err(out_of_gas) {
                    return (Err(e.into()), gas_used);
                }
            }
            _ => working.abort(),
        };
        let _ = instance.recycle();

        (result, gas_used)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn query(
        &self,
        checksum: &Checksum,
        env: &Env,
        msg: &[u8],
        global_storage: &dyn ReadonlyStorage,
        contract_addr: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> (Result<Result<Binary, String>, VmError>, u64) {
        let gas_limit = sdk_gas_to_wasmer(meter.remaining());
        let options = InstanceOptions {
            gas_limit,
            print_debug: self.print_debug,
        };

        // Create WeakSubTx that only holds readable access, so we can query underlying storage as contract is working
        let mut scratch = ScratchTx::new(global_storage);
        let mut unmetered = contract_storage(&mut scratch, contract_addr);
        let mut contract = AppMeter::new(&mut unmetered);

        // This is where we fake all the lifetimes....
        let backend =
            unsafe { danger_will_robinson(sm, &mut contract, global_storage, meter, &env.block) };

        let mut instance = match self.cache.get_instance(checksum, backend, options) {
            Ok(i) => i,
            // No gas used yet
            Err(e) => return (Err(e), 0),
        };

        // execute the contract and get gas_used
        instance.set_storage_readonly(false);
        let result = call_query(&mut instance, env, msg);
        let result = result.map(|x| x.into_result());
        let gas_used = wasmer_gas_to_sdk(instance.create_gas_report().used_internally);

        // always abort scratch, as we don't want to commit anything
        scratch.abort();
        let _ = instance.recycle();

        (result, gas_used)
    }
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::{
        coin, from_json,
        testing::{mock_env, mock_info},
        to_json_vec, Order, Uint128,
    };
    use cw20::Cw20Coin;
    use layer_std::AccountId;
    use layer_storage::{MemoryStore, PersistentStorage};

    use crate::AppConfig;

    use super::*;

    // v1.0.1
    const CW20_BASE: &[u8] = include_bytes!("../../../fixtures/cw20_base.wasm");

    #[test]
    fn can_instatiate() {
        let path = "/tmp/slay3r/test-can-instantiate";
        let _ = std::fs::remove_dir_all(path);
        std::fs::create_dir_all(path).unwrap();

        let vm = VmCache::init(path);
        let (checksum, _) = vm.store_code(CW20_BASE).unwrap();

        // try to instantiate
        let env = mock_env();
        let sender = AccountId::unchecked("Sillyness");
        let contract = AccountId::unchecked("My first cw20");
        let info = mock_info(&sender.to_string(), &[coin(55_000, "uslay")]);
        let meter = GasMeter::infinite();
        let sm = StateMachine::new(&AppConfig::new(path));
        let store = MemoryStore::new();

        let msg = cw20_base::msg::InstantiateMsg {
            name: "slayer".to_string(),
            symbol: "SLAY".to_string(),
            decimals: 6,
            initial_balances: vec![Cw20Coin {
                address: sender.to_string(),
                amount: Uint128::new(1234567),
            }],
            mint: None,
            marketing: None,
        };
        let msg = to_json_vec(&msg).unwrap();

        let mut writer = store.writer();
        let (res, gas_used) = vm.instantiate(
            &checksum,
            &env,
            &info,
            &msg,
            &mut writer,
            &contract,
            &meter,
            &sm,
        );
        let res = res.unwrap().unwrap();
        assert_eq!(res.messages.len(), 0);
        assert_eq!(res.events.len(), 0);
        assert_eq!(res.attributes.len(), 0);
        assert_eq!(gas_used, 56);

        // query the state was written - token_info and total supply
        let num = writer
            .range(&meter, None, None, Order::Ascending)
            .unwrap()
            .count();
        assert_eq!(num, 3);
    }

    #[test]
    fn happy_path_create_send_query() {
        let path = "/tmp/slay3r/test-happy-path-create-send-query";
        let _ = std::fs::remove_dir_all(path);
        std::fs::create_dir_all(path).unwrap();

        let mut vm = VmCache::init(path);
        let (checksum, _) = vm.store_code(CW20_BASE).unwrap();

        // try to instantiate
        let env = mock_env();
        let sender = AccountId::unchecked("Sillyness");
        let contract = AccountId::unchecked("My Token");
        let info = mock_info(&sender.to_string(), &[]);
        let meter = GasMeter::infinite();
        let sm = StateMachine::new(&AppConfig::new(path));
        let store = MemoryStore::new();
        let mut writer = store.writer();

        // instantiate
        let msg = cw20_base::msg::InstantiateMsg {
            name: "slayer".to_string(),
            symbol: "SLAY".to_string(),
            decimals: 6,
            initial_balances: vec![Cw20Coin {
                address: sender.to_string(),
                amount: Uint128::new(1234567),
            }],
            mint: None,
            marketing: None,
        };
        let msg = to_json_vec(&msg).unwrap();
        let (res, _) = vm.instantiate(
            &checksum,
            &env,
            &info,
            &msg,
            &mut writer,
            &contract,
            &meter,
            &sm,
        );
        let _ = res.unwrap().unwrap();

        // query two addresses
        let rcpt = AccountId::unchecked("Bystander");
        let hero = query_balance(
            &mut vm,
            &checksum,
            &env,
            &sender,
            writer.as_ref(),
            &contract,
            &meter,
            &sm,
        );
        assert_eq!(hero.u128(), 1234567u128);
        let zero = query_balance(
            &mut vm,
            &checksum,
            &env,
            &rcpt,
            writer.as_ref(),
            &contract,
            &meter,
            &sm,
        );
        assert_eq!(zero.u128(), 0u128);

        // send some surpise tokens
        let msg = cw20_base::msg::ExecuteMsg::Transfer {
            recipient: rcpt.to_string(),
            amount: Uint128::new(23456),
        };
        let msg = to_json_vec(&msg).unwrap();
        let (res, _) = vm.execute(
            &checksum,
            &env,
            &info,
            &msg,
            &mut writer,
            &contract,
            &meter,
            &sm,
        );
        let _ = res.unwrap().unwrap();

        // query two addresses wirh new balances
        let rcpt = AccountId::unchecked("Bystander");
        let hero = query_balance(
            &mut vm,
            &checksum,
            &env,
            &sender,
            writer.as_ref(),
            &contract,
            &meter,
            &sm,
        );
        assert_eq!(hero.u128(), 1211111u128);
        let zero = query_balance(
            &mut vm,
            &checksum,
            &env,
            &rcpt,
            writer.as_ref(),
            &contract,
            &meter,
            &sm,
        );
        assert_eq!(zero.u128(), 23456u128);
    }

    #[test]
    fn query_with_iterator() {
        let path = "/tmp/slay3r/test-query-with-iterator";
        let _ = std::fs::remove_dir_all(path);
        std::fs::create_dir_all(path).unwrap();

        let vm = VmCache::init(path);
        let (checksum, _) = vm.store_code(CW20_BASE).unwrap();

        // try to instantiate
        let env = mock_env();
        let one = AccountId::unchecked("One");
        let two = AccountId::unchecked("Two");
        let three = AccountId::unchecked("Xyz");
        let contract = AccountId::unchecked("Another Token");
        let info = mock_info(&one.to_string(), &[]);
        let meter = GasMeter::infinite();
        let sm = StateMachine::new(&AppConfig::new(path));
        let store = MemoryStore::new();
        let mut writer = store.writer();

        // instantiate
        let msg = cw20_base::msg::InstantiateMsg {
            name: "slayer".to_string(),
            symbol: "SLAY".to_string(),
            decimals: 6,
            initial_balances: vec![
                Cw20Coin {
                    address: one.to_string(),
                    amount: Uint128::new(1234567),
                },
                Cw20Coin {
                    address: two.to_string(),
                    amount: Uint128::new(7654321),
                },
                Cw20Coin {
                    address: three.to_string(),
                    amount: Uint128::new(818818),
                },
            ],
            mint: None,
            marketing: None,
        };
        let msg = to_json_vec(&msg).unwrap();
        let (res, _) = vm.instantiate(
            &checksum,
            &env,
            &info,
            &msg,
            &mut writer,
            &contract,
            &meter,
            &sm,
        );
        let _ = res.unwrap().unwrap();

        // now list all accounts
        let msg = cw20_base::msg::QueryMsg::AllAccounts {
            start_after: None,
            limit: None,
        };
        let msg = to_json_vec(&msg).unwrap();
        let (res, _) = vm.query(
            &checksum,
            &env,
            &msg,
            writer.as_ref(),
            &contract,
            &meter,
            &sm,
        );
        let res = res.unwrap().unwrap();
        let cw20::AllAccountsResponse { accounts } = from_json(res).unwrap();
        assert_eq!(
            accounts,
            vec![one.to_string(), two.to_string(), three.to_string()]
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn query_balance(
        vm: &mut VmCache,
        checksum: &Checksum,
        env: &Env,
        account: &AccountId,
        global_storage: &dyn ReadonlyStorage,
        contract_addr: &AccountId,
        meter: &GasMeter,
        sm: &StateMachine,
    ) -> Uint128 {
        let msg = cw20_base::msg::QueryMsg::Balance {
            address: account.to_string(),
        };
        let msg = to_json_vec(&msg).unwrap();
        let (res, _) = vm.query(
            checksum,
            env,
            &msg,
            global_storage,
            contract_addr,
            meter,
            sm,
        );
        let res = res.unwrap().unwrap();
        let balance: cw20::BalanceResponse = from_json(res).unwrap();
        balance.balance
    }
}
