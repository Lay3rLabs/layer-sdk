use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    ensure_eq, to_json_binary, Addr, Binary, BlockInfo, Coin, CosmosMsg, Empty, Env, MessageInfo,
    Order, Reply, ReplyOn, SubMsg, SubMsgResponse,
};
// 2.0: use cosmwasm_std::Checksum
use cosmwasm_vm::{Checksum, VmError};
use cw_storage_plus::{Bound, KeyDeserialize};

use layer_std::api::MsgResponse;
use layer_std::response::{
    CodeInfoResponse, ContractInfoResponse, ContractsByCodeResponse, ListCodesResponse,
    QueryResponse, WasmQueryResponse,
};
use layer_std::root::CustomRootMsg;
use layer_std::{
    AccountId, BankMsgData, GasError, GasMeter, Msg, MsgData, WasmMsg, WasmMsgData, WasmQuery,
};
use layer_storage::{
    prefixed, prefixed_read, Item, Map, PlusError, PrefixedStorage, ReadonlyPrefixedStorage,
    ReadonlyStorage, Storage,
};

use super::events::{migrate_event, reply_event};
use super::utils::build_instantiate_address;
use super::vm::VmCache;
use super::WasmError;
use crate::error::{PulsarError, PulsarResult};
use crate::sm::StateMachine;
use crate::wasm::events::{
    build_contract_events, clear_admin_event, execute_event, instantiate_event, pin_code_event,
    store_code_event, sudo_event, unpin_code_event, update_admin_event,
};
use crate::wasm::utils::build_instantiate_2_address;

pub const NAMESPACE_WASM: &[u8] = b"wasm";

pub const ROOT_ADDR: [u8; 20] = hex_literal::hex!("0da01da02da03da04da05da06da07da08da09da0");

// Note: to update this, run ./scripts/build_contracts.sh from workspace root
const ROOT_WASM: &[u8] = include_bytes!("../../fixtures/layer_root.wasm");

pub fn root_account() -> AccountId {
    AccountId::new(&ROOT_ADDR).unwrap()
}

// Numbers taken from wasmd, we should benchmark better
pub(crate) const LOAD_WASM_GAS: u64 = 60_000;
pub(crate) const LOAD_PINNED_WASM_GAS: u64 = 2_000;

// Contract state is kept in Storage, separate from the contracts themselves
const CODES: Map<u64, CodeInfo> = Map::new("codes");
const CONTRACTS: Map<&AccountId, ContractData> = Map::new("contracts");
const CONTRACTS_BY_CODE: Map<(u64, &AccountId), bool> = Map::new("contracts_by_code");

// list of all pinned code_ids, so you can range over them
const PINNED: Map<u64, Empty> = Map::new("pinned");
// latest code id
const CODE_ID: Item<u64> = Item::new("code_id");
// A counter of total contracts created for the v1 instantiate algorithm
const CONTRACT_COUNTER: Item<u64> = Item::new("contract_count");

// TODO: ideally we can derive these from the actual buckets for no typos.
// But for now, this is easier to write than building some auto-magic framework
pub fn parse_keys(bucket: &str, key: Vec<u8>) -> Vec<String> {
    match bucket {
        "codes" => vec![u64::from_vec(key).unwrap().to_string()],
        "contracts" => vec![AccountId::from_vec(key).unwrap().to_string()],
        "contracts_by_code" => {
            let (id, contract) = <(u64, AccountId)>::from_vec(key).unwrap();
            vec![id.to_string(), contract.to_string()]
        }
        "pinned" => vec![u64::from_vec(key).unwrap().to_string()],
        "code_id" => vec![],
        "contract_count" => vec![],
        "params" => vec![],
        // anything else will be a contracts internal storage, we cannot parse more.
        // just hex-encode it for the recipient to parse in a more meaningful way
        _ => vec![hex::encode(&key)],
    }
}

/// Contract Data includes information about contract, equivalent of `ContractInfo` in wasmd
/// interface.
#[cw_serde]
pub struct ContractData {
    /// Identifier of stored contract code
    pub code_id: u64,
    /// Address of account who initially instantiated the contract
    pub creator: AccountId,
    /// Optional address of account who can execute migrations
    pub admin: Option<AccountId>,
    /// Metadata passed while contract instantiation
    pub label: String,
    /// Blockchain height in the moment of instantiating the contract
    pub created: u64,
    // LATER: add ibc info
}

#[cw_serde]
pub struct CodeInfo {
    /// The address that initially stored the code
    pub creator: AccountId,
    /// The hash of the Wasm blob
    pub checksum: Binary,
    /// If it is pinned or not
    pub pinned: bool,
}

impl CodeInfo {
    /// This gets a proper checksum to load a wasmer vm, and charges a gas price based on whether it is pinned or not
    fn get_checksum_to_execute(&self, meter: &GasMeter) -> Result<Checksum, GasError> {
        if self.pinned {
            meter.charge(LOAD_PINNED_WASM_GAS)?;
        } else {
            meter.charge(LOAD_WASM_GAS)?;
        }
        Ok(self.get_checksum_not_executing())
    }
    /// Use this is you need Checksum to interact with the cache, but not run wasmer vm, like pin/unpin
    fn get_checksum_not_executing(&self) -> Checksum {
        Checksum::try_from(self.checksum.as_slice()).unwrap()
    }
}

#[derive(Debug)]
pub struct Wasm {
    cache: VmCache,
}

/// This can be set different on each node, outside of consensus
#[derive(Debug, Clone)]
pub struct WasmConfig {
    pub cache_dir: String,
}

impl Wasm {
    pub fn new(config: &WasmConfig) -> Self {
        // ensure path exists
        let path = config.cache_dir.as_str();
        std::fs::create_dir_all(path).unwrap();
        Wasm {
            cache: VmCache::init(path),
        }
    }

    /// This does storage-dependent initialization...
    /// In particular, it installs the root contract and initializes it at the predefined address.
    pub fn init(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        params: crate::genesis::WasmParams,
        sm: &StateMachine,
    ) -> PulsarResult<()> {
        let gov_account = AccountId::parse_string(&params.gov_account)?;

        // store the root code (copied code from _process_msg / WasmMsg::StoreCode)
        let (code_id, code) = {
            let mut wasm_store = prefixed(storage, NAMESPACE_WASM);
            let (checksum, _) = self.cache.store_code(ROOT_WASM).map_err(map_vm_error)?;
            let info = CodeInfo {
                creator: gov_account.clone(),
                checksum: Vec::<u8>::from(checksum).into(),
                pinned: true,
            };
            let id = self.next_id(&mut wasm_store, meter)?;
            CODES.save(&mut wasm_store, meter, id, &info)?;

            // now, pin it (copied code from _process_msg / WasmMsg::Pin)
            self.cache
                .pin(&info.get_checksum_not_executing())
                .map_err(map_vm_error)?;
            PINNED.save(&mut wasm_store, meter, id, &Empty {})?;

            (id, info)
        };

        // now, let's instantiate the root contract, with the given gov address
        let msg = to_json_binary(&layer_std::root::InstantiateMsg {
            gov_address: gov_account.to_string(),
        })?;
        let _res = self.do_instantiate(
            storage,
            meter,
            block,
            sm,
            &gov_account,
            root_account(),
            code_id,
            code,
            Some(gov_account.clone()),
            msg,
            vec![],
            "Root Contract".to_string(),
            true,
        )?;
        Ok(())
    }

    /// This is v1 contract address generation
    fn generate_address(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        sender: &AccountId,
        code_id: u64,
    ) -> Result<AccountId, PulsarError> {
        let mut wasm_store = prefixed(storage, NAMESPACE_WASM);
        let counter = CONTRACT_COUNTER
            .may_load(wasm_store.as_ref(), meter)?
            .unwrap_or_default()
            + 1;
        CONTRACT_COUNTER.save(&mut wasm_store, meter, &counter)?;
        build_instantiate_address(sender, code_id, counter)
    }

    fn next_id(&self, wasm_store: &mut dyn Storage, meter: &GasMeter) -> Result<u64, PulsarError> {
        let id = CODE_ID
            .may_load(wasm_store.as_ref(), meter)?
            .unwrap_or_default()
            + 1;
        CODE_ID.save(wasm_store, meter, &id)?;
        Ok(id)
    }

    pub fn process_msg(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        signer: &AccountId,
        msg: WasmMsg,
    ) -> PulsarResult<MsgResponse> {
        self._process_msg(storage, meter, block, sm, signer, msg, true)
    }

    pub fn process_msg_no_submsg(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        signer: &AccountId,
        msg: WasmMsg,
    ) -> PulsarResult<MsgResponse> {
        self._process_msg(storage, meter, block, sm, signer, msg, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn _process_msg(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        signer: &AccountId,
        msg: WasmMsg,
        allow_submsg: bool,
    ) -> PulsarResult<MsgResponse> {
        let resp = match msg {
            WasmMsg::StoreCode { sender, code } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let (checksum, analysis) = self.cache.store_code(&code).map_err(map_vm_error)?;
                let info = CodeInfo {
                    creator: sender,
                    checksum: Vec::<u8>::from(checksum).into(),
                    pinned: false,
                };
                let mut wasm_store = prefixed(storage, NAMESPACE_WASM);
                let id = self.next_id(&mut wasm_store, meter)?;

                self.save_code(storage, meter, id, &info)?;
                let data = WasmMsgData::Store {
                    code_id: id,
                    checksum: info.checksum,
                };
                let event = store_code_event(id, analysis);
                MsgResponse::new(vec![event], data)
            }
            WasmMsg::Instantiate {
                sender,
                admin,
                code_id,
                msg,
                funds,
                label,
            } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let code = self.load_code(storage.as_ref(), meter, code_id)?;
                let contract_addr = self.generate_address(storage, meter, &sender, code_id)?;
                return self.do_instantiate(
                    storage,
                    meter,
                    block,
                    sm,
                    signer,
                    contract_addr,
                    code_id,
                    code,
                    admin,
                    msg,
                    funds,
                    label,
                    allow_submsg,
                );
            }
            WasmMsg::Instantiate2 {
                sender,
                admin,
                code_id,
                label,
                msg,
                funds,
                salt,
            } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let code = self.load_code(storage.as_ref(), meter, code_id)?;
                let contract_addr = build_instantiate_2_address(
                    &code.checksum,
                    &sender,
                    &salt,
                    b"", // we consider fix_msg to always be false, this was cosmwasm-std decision
                )?;
                return self.do_instantiate(
                    storage,
                    meter,
                    block,
                    sm,
                    signer,
                    contract_addr,
                    code_id,
                    code,
                    admin,
                    msg,
                    funds,
                    label,
                    allow_submsg,
                );
            }
            WasmMsg::Execute {
                sender,
                contract_addr,
                msg,
                funds,
            } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let contract = self.load_contract(storage.as_ref(), meter, &contract_addr)?;
                let code = self.load_code(storage.as_ref(), meter, contract.code_id)?;

                // send funds
                if !funds.is_empty() {
                    // TODO: this needs to update to handle cw20 as well
                    sm.bank.transfer(
                        storage,
                        meter,
                        block,
                        sm,
                        sender.clone(),
                        contract_addr.clone(),
                        funds.clone(),
                    )?;
                }
                let info = build_info(&sender, funds);

                // call execute on cache
                let env = build_env(block, &contract_addr);
                let checksum = code.get_checksum_to_execute(meter)?;
                let (result, gas) = self.cache.execute(
                    &checksum,
                    &env,
                    &info,
                    &msg,
                    storage,
                    &contract_addr,
                    meter,
                    sm,
                );
                meter.charge(gas)?;
                let result = map_cache_result(result)?;

                // Build events
                let mut events =
                    build_contract_events(&contract_addr, result.events, result.attributes)?;
                let event = execute_event(&contract_addr);
                events.insert(0, event);

                // Dispatch messages
                let data = WasmMsgData::Execute {
                    data: result.data.unwrap_or_default(),
                };
                let mut response = MsgResponse::new(events, data);
                self.dispatch_response_messages(
                    storage,
                    meter,
                    block,
                    sm,
                    &contract_addr,
                    &checksum,
                    result.messages,
                    &mut response,
                    allow_submsg,
                )?;
                response
            }
            WasmMsg::Migrate {
                sender,
                contract_addr,
                new_code_id,
                msg,
            } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let root = root_account();
                // Only admin or root can call
                let mut contract = self.load_contract(storage.as_ref(), meter, &contract_addr)?;
                match &contract.admin {
                    Some(admin) if admin == &sender => Ok(()),
                    _ if sender == root => Ok(()),
                    _ => Err(WasmError::Unauthorized),
                }?;

                // update the code and get the new code info
                self.remove_contract_by_code(storage, meter, &contract_addr, &contract)?;
                let code = self.load_code(storage.as_ref(), meter, new_code_id)?;
                contract.code_id = new_code_id;
                self.save_contract(storage, meter, &contract_addr, &contract)?;
                self.save_contract_by_code(storage, meter, &contract_addr, &contract)?;

                // call migrate on vm
                let env = build_env(block, &contract_addr);
                let checksum = code.get_checksum_to_execute(meter)?;
                let (result, gas) =
                    self.cache
                        .migrate(&checksum, &env, &msg, storage, &contract_addr, meter, sm);
                meter.charge(gas)?;
                let result = map_cache_result(result)?;

                // Build events
                let mut events =
                    build_contract_events(&contract_addr, result.events, result.attributes)?;
                let event = migrate_event(&contract_addr, new_code_id);
                events.insert(0, event);

                // dispatch messages
                let data = WasmMsgData::Migrate {
                    data: result.data.unwrap_or_default(),
                };
                let mut response = MsgResponse::new(events, data);
                self.dispatch_response_messages(
                    storage,
                    meter,
                    block,
                    sm,
                    &contract_addr,
                    &checksum,
                    result.messages,
                    &mut response,
                    allow_submsg,
                )?;
                response
            }
            WasmMsg::ClearAdmin {
                sender,
                contract_addr,
            } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let root = root_account();
                let mut contract = self.load_contract(storage.as_ref(), meter, &contract_addr)?;
                // Only admin or root can call
                match &contract.admin {
                    Some(admin) if admin == &sender => Ok(()),
                    _ if sender == root => Ok(()),
                    _ => Err(WasmError::Unauthorized),
                }?;
                contract.admin = None;
                self.save_contract(storage, meter, &contract_addr, &contract)?;
                let event = clear_admin_event(&contract_addr);
                MsgResponse::new(vec![event], WasmMsgData::ClearAdmin {})
            }
            WasmMsg::UpdateAdmin {
                sender,
                contract_addr,
                admin,
            } => {
                ensure_eq!(signer, &sender, WasmError::SenderMismatch);
                let root = root_account();
                // Only admin or root can call
                let mut contract = self.load_contract(storage.as_ref(), meter, &contract_addr)?;
                match &contract.admin {
                    Some(admin) if admin == &sender => Ok(()),
                    _ if sender == root => Ok(()),
                    _ => Err(WasmError::Unauthorized),
                }?;
                let event = update_admin_event(&contract_addr, &admin);
                contract.admin = Some(admin);
                self.save_contract(storage, meter, &contract_addr, &contract)?;
                MsgResponse::new(vec![event], WasmMsgData::UpdateAdmin {})
            }
            WasmMsg::Sudo {
                sender,
                contract_addr,
                msg,
            } => {
                // Only root can call
                let root = root_account();
                ensure_eq!(sender, root, WasmError::NotRoot);

                let contract = self.load_contract(storage.as_ref(), meter, &contract_addr)?;
                let code = self.load_code(storage.as_ref(), meter, contract.code_id)?;

                // call sudo on vm
                let env = build_env(block, &contract_addr);
                let checksum = code.get_checksum_to_execute(meter)?;
                let (result, gas) =
                    self.cache
                        .sudo(&checksum, &env, &msg, storage, &contract_addr, meter, sm);
                meter.charge(gas)?;
                let result = map_cache_result(result)?;

                // Build events
                let mut events =
                    build_contract_events(&contract_addr, result.events, result.attributes)?;
                let event = sudo_event(&contract_addr);
                events.insert(0, event);

                // dispatch messages
                let data = WasmMsgData::Sudo {
                    data: result.data.unwrap_or_default(),
                };
                let mut response = MsgResponse::new(events, data);
                self.dispatch_response_messages(
                    storage,
                    meter,
                    block,
                    sm,
                    &contract_addr,
                    &checksum,
                    result.messages,
                    &mut response,
                    allow_submsg,
                )?;
                response
            }
            WasmMsg::Pin { sender, code_id } => {
                // Only root can call
                let root = root_account();
                ensure_eq!(sender, root, WasmError::NotRoot);

                let mut code = self.load_code(storage.as_ref(), meter, code_id)?;
                if !code.pinned {
                    code.pinned = true;
                    self.save_code(storage, meter, code_id, &code)?;
                    self.cache
                        .pin(&code.get_checksum_not_executing())
                        .map_err(map_vm_error)?;
                    PINNED.save(
                        &mut prefixed(storage, NAMESPACE_WASM),
                        meter,
                        code_id,
                        &Empty {},
                    )?;
                }
                // add events
                let event = pin_code_event(code_id);
                MsgResponse::new(vec![event], WasmMsgData::PinCode {})
            }
            WasmMsg::Unpin { sender, code_id } => {
                // Only root can call
                let root = root_account();
                ensure_eq!(sender, root, WasmError::NotRoot);

                let mut code = self.load_code(storage.as_ref(), meter, code_id)?;
                if code.pinned {
                    code.pinned = false;
                    self.save_code(storage, meter, code_id, &code)?;
                    self.cache
                        .unpin(&code.get_checksum_not_executing())
                        .map_err(map_vm_error)?;
                    PINNED.remove(&mut prefixed(storage, NAMESPACE_WASM), meter, code_id)?;
                }
                // add events
                let event = unpin_code_event(code_id);
                MsgResponse::new(vec![event], WasmMsgData::UnpinCode {})
            }
        };
        Ok(resp)
    }

    /// Internal function only, combining instantiate and instantiate2 common path.
    /// I know there are way too many args, but no one should use this besides two cases
    /// right above it....
    #[allow(clippy::too_many_arguments)]
    fn do_instantiate(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        signer: &AccountId,
        contract_addr: AccountId,
        code_id: u64,
        code: CodeInfo,
        admin: Option<AccountId>,
        msg: Binary,
        funds: Vec<Coin>,
        label: String,
        allow_submsg: bool,
    ) -> PulsarResult<MsgResponse> {
        let sender = signer;

        // reserve and auth account and ensure it is not already taken
        sm.auth
            .claim_internal_account(storage, meter, &contract_addr)?;

        // save contract
        let contract = ContractData {
            code_id,
            creator: sender.clone(),
            admin,
            label,
            created: block.height,
        };
        self.save_contract(storage, meter, &contract_addr, &contract)?;
        self.save_contract_by_code(storage, meter, &contract_addr, &contract)?;

        // send funds
        if !funds.is_empty() {
            sm.bank.transfer(
                storage,
                meter,
                block,
                sm,
                sender.clone(),
                contract_addr.clone(),
                funds.clone(),
            )?;
        }
        let info = build_info(sender, funds);

        // call instantiate on cache
        let env = build_env(block, &contract_addr);
        let checksum = code.get_checksum_to_execute(meter)?;
        let (result, gas) = self.cache.instantiate(
            &checksum,
            &env,
            &info,
            &msg,
            storage,
            &contract_addr,
            meter,
            sm,
        );
        meter.charge(gas)?;
        let result = map_cache_result(result)?;

        // Build events
        let mut events = build_contract_events(&contract_addr, result.events, result.attributes)?;
        let event = instantiate_event(&contract_addr, code_id);
        events.insert(0, event);

        // dispatch messages
        let data = WasmMsgData::Instantiate {
            contract: contract_addr.clone(),
            data: result.data.unwrap_or_default(),
        };
        let mut response = MsgResponse::new(events, data);
        self.dispatch_response_messages(
            storage,
            meter,
            block,
            sm,
            &contract_addr,
            &checksum,
            result.messages,
            &mut response,
            allow_submsg,
        )?;
        Ok(response)
    }

    /// This dispatches all returned messages and adds events to the parent event of the original call
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_response_messages(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        contract: &AccountId,
        checksum: &Checksum,
        msgs: Vec<SubMsg<super::vm::CustomMsg>>,
        // we append events to this (later maybe overwrite the data)
        parent_response: &mut MsgResponse,
        // if set, we error if msgs is not empty
        allow_submsg: bool,
    ) -> PulsarResult<()> {
        if !allow_submsg && !msgs.is_empty() {
            return Err(WasmError::SubMsgNotSupported.into());
        }

        for msg in msgs {
            // if there is a limit, and it is less than what we have left, use a sub-meter
            let sub_meter = match (msg.gas_limit, meter.remaining()) {
                (Some(limit), left) if limit < left => GasMeter::new(limit),
                (_, left) => GasMeter::new(left),
            };
            let layer_msg = cosmwasm_msg_to_layer(msg.msg, contract)?;

            // ensure we charge if there is a limit_meter, even on error
            let msg_result = sm.process_msg(storage, &sub_meter, contract, block, layer_msg);
            let gas_used = sub_meter.used();
            meter.charge(gas_used)?;

            // check if we want to call reply and call
            let is_success = msg_result.is_ok(); // we use this variable later
            let handle_reply = matches!(
                (msg.reply_on, is_success),
                (ReplyOn::Always, _) | (ReplyOn::Success, true) | (ReplyOn::Error, false)
            );
            if handle_reply {
                let result = match msg_result {
                    Ok(res) => {
                        // append events to parent response (only on success)
                        parent_response.events.extend(res.events.clone());
                        // and prepare a response value to call the contract with both (1.x) data and (2.x) msg_responses
                        let (_type_url, value) = encode_cosmwasm_response(res.data);
                        #[allow(deprecated)]
                        let data = maybe_binary(value.clone());
                        // 2.0:
                        // let msg_response = cosmwasm_std::MsgResponse {
                        //     type_url: type_url.to_string(),
                        //     value: value.into(),
                        // };
                        #[allow(deprecated)]
                        Ok(SubMsgResponse {
                            events: res.events,
                            data,
                            // msg_responses: vec![msg_response],
                        })
                    }
                    Err(err) => Err(err.to_string()),
                };
                let reply = Reply {
                    id: msg.id,
                    result: result.into(),
                    // 2.0:
                    // gas_used,
                    // payload: msg.payload,
                };

                // call the reply entry point
                let env = build_env(block, contract);
                let (reply_result, gas) = self
                    .cache
                    .reply(checksum, &env, &reply, storage, contract, meter, sm);
                meter.charge(gas)?;
                let reply_result = map_cache_result(reply_result)?;

                // Append reply events to the parent
                parent_response
                    .events
                    .push(reply_event(contract, is_success));
                let events =
                    build_contract_events(contract, reply_result.events, reply_result.attributes)?;
                parent_response.events.extend(events);

                // if data is set, then we override the parent data field
                if let Some(data) = reply_result.data {
                    // parent must be Execute, Instantiate(2), Migrate, or Sudo
                    // update the data field but leave the type the same
                    set_data_field(&mut parent_response.data, data);
                }

                if !reply_result.messages.is_empty() {
                    // we just run them all and add events to the parent...
                    self.dispatch_response_messages(
                        storage,
                        meter,
                        block,
                        sm,
                        contract,
                        checksum,
                        reply_result.messages,
                        parent_response,
                        allow_submsg,
                    )?;
                }
            } else {
                // add events to parent (when we don't use reply)
                let res = msg_result?;
                parent_response.events.extend(res.events);
            }
        }
        Ok(())
    }

    pub fn query(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        request: WasmQuery,
    ) -> PulsarResult<QueryResponse<PulsarError>> {
        let resp = match request {
            WasmQuery::Smart { contract_addr, msg } => {
                let contract = self.load_contract(storage, meter, &contract_addr)?;
                let code = self.load_code(storage, meter, contract.code_id)?;
                let checksum = code.get_checksum_to_execute(meter)?;
                let env = build_env(block, &contract_addr);
                let (result, gas) =
                    self.cache
                        .query(&checksum, &env, &msg, storage, &contract_addr, meter, sm);
                meter.charge(gas)?;
                let result = map_cache_result(result)?;
                WasmQueryResponse::Smart(result)
            }
            WasmQuery::Raw { contract_addr, key } => {
                let sub_store = read_contract_storage(storage, &contract_addr);
                let data = sub_store.get(meter, &key)?.unwrap_or_default();
                WasmQueryResponse::Raw(data.into())
            }
            WasmQuery::ContractInfo { contract_addr } => {
                let ContractData {
                    code_id,
                    creator,
                    admin,
                    label,
                    created,
                } = self.load_contract(storage, meter, &contract_addr)?;
                let CodeInfo { pinned, .. } = self.load_code(storage, meter, code_id)?;
                let resp = ContractInfoResponse {
                    addresss: contract_addr,
                    code_id,
                    creator,
                    admin,
                    ibc_port: None,
                    pinned,
                    label,
                    created,
                };
                WasmQueryResponse::ContractInfo(resp)
            }
            WasmQuery::CodeInfo {
                code_id,
                include_wasm,
            } => {
                let CodeInfo {
                    creator,
                    checksum,
                    pinned,
                } = self.load_code(storage, meter, code_id)?;
                let hash = Checksum::try_from(checksum.as_slice()).map_err(map_vm_error)?;
                let data = if include_wasm {
                    self.cache.load_code(&hash).map_err(map_vm_error)?
                } else {
                    vec![]
                };
                let resp = CodeInfoResponse {
                    data: data.into(),
                    code_info: layer_std::response::CodeInfo {
                        code_id,
                        creator,
                        checksum,
                        pinned,
                    },
                };
                WasmQueryResponse::CodeInfo(resp)
            }
            WasmQuery::ListCodes { from, limit } => {
                let start = from.map(Bound::inclusive);
                let limit = limit.unwrap_or(100u32) as u64; // max page size // TODO: configure??
                let end = Some(Bound::exclusive(from.unwrap_or(0) + limit));

                let reader = prefixed_read(storage, NAMESPACE_WASM);
                let iter = CODES.range(&reader, meter, start, end, Order::Ascending)?;
                let code_infos = iter
                    .map(|r| {
                        r.map(|(k, v)| layer_std::response::CodeInfo {
                            code_id: k,
                            creator: v.creator,
                            checksum: v.checksum,
                            pinned: v.pinned,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                WasmQueryResponse::ListCodes(ListCodesResponse { code_infos })
            }
            WasmQuery::ContractsByCode { code_id } => {
                // Uses a manually tracked secondary index...
                let wasm_store = prefixed_read(storage, NAMESPACE_WASM);
                let contracts = CONTRACTS_BY_CODE
                    .prefix(code_id)
                    .range(&wasm_store, meter, None, None, Order::Ascending)?
                    .map(|r| r.map(|(k, _)| k))
                    .collect::<Result<Vec<_>, _>>()?;
                let resp = ContractsByCodeResponse { contracts };
                WasmQueryResponse::ContractsByCode(resp)
            }
        };
        Ok(QueryResponse::Wasm(resp))
    }

    fn load_contract(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        address: &AccountId,
    ) -> Result<ContractData, PlusError> {
        CONTRACTS.load(&prefixed_read(storage, NAMESPACE_WASM), meter, address)
    }

    fn save_contract(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        address: &AccountId,
        contract: &ContractData,
    ) -> Result<(), PlusError> {
        let mut wasm_store = prefixed(storage, NAMESPACE_WASM);
        CONTRACTS.save(&mut wasm_store, meter, address, contract)
    }

    fn save_contract_by_code(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        address: &AccountId,
        contract: &ContractData,
    ) -> Result<(), PlusError> {
        let mut wasm_store = prefixed(storage, NAMESPACE_WASM);
        // We need to manually remove old code id entry in migrate... but this handles a lot of the tracking
        CONTRACTS_BY_CODE.save(&mut wasm_store, meter, (contract.code_id, address), &true)
    }

    // call this on migrate of whenever a contract will have a new code_id
    fn remove_contract_by_code(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        address: &AccountId,
        contract: &ContractData,
    ) -> Result<(), GasError> {
        let mut wasm_store = prefixed(storage, NAMESPACE_WASM);
        CONTRACTS_BY_CODE.remove(&mut wasm_store, meter, (contract.code_id, address))
    }

    fn load_code(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        id: u64,
    ) -> Result<CodeInfo, PlusError> {
        CODES.load(&prefixed_read(storage, NAMESPACE_WASM), meter, id)
    }

    fn save_code(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        id: u64,
        info: &CodeInfo,
    ) -> Result<(), PlusError> {
        CODES.save(&mut prefixed(storage, NAMESPACE_WASM), meter, id, info)
    }
}

pub fn contract_storage<'a>(storage: &'a mut dyn Storage, addr: &AccountId) -> PrefixedStorage<'a> {
    PrefixedStorage::multilevel(storage, &[NAMESPACE_WASM, addr.as_slice()])
}

pub fn read_contract_storage<'a>(
    storage: &'a dyn ReadonlyStorage,
    addr: &AccountId,
) -> ReadonlyPrefixedStorage<'a> {
    ReadonlyPrefixedStorage::multilevel(storage, &[NAMESPACE_WASM, addr.as_slice()])
}

fn build_env(block: &BlockInfo, contract: &AccountId) -> Env {
    Env {
        block: block.clone(),
        transaction: None, // review later
        contract: cosmwasm_std::ContractInfo {
            address: Addr::unchecked(contract.to_string()),
        },
    }
}

fn build_info(sender: &AccountId, funds: Vec<Coin>) -> MessageInfo {
    MessageInfo {
        sender: Addr::unchecked(sender.to_string()),
        funds,
    }
}

fn map_vm_error(err: VmError) -> PulsarError {
    match err {
        VmError::GasDepletion { .. } => GasError::OutOfGas.into(),
        // FIXME: make this deterministic
        e => WasmError::Vm(e.to_string()).into(),
    }
}

// 2.0:
// fn map_checksum_error(_: cosmwasm_std::ChecksumError) -> PulsarError {
//     PulsarError::Wasm(WasmError::Checksum)
// }

fn map_contract_error(err: String) -> PulsarError {
    WasmError::Contract(err).into()
}

fn map_cache_result<T>(result: Result<Result<T, String>, VmError>) -> Result<T, PulsarError> {
    result.map_err(map_vm_error)?.map_err(map_contract_error)
}

fn cosmwasm_msg_to_layer(
    msg: CosmosMsg<super::vm::CustomMsg>,
    sender: &AccountId,
) -> Result<Msg, PulsarError> {
    let res = match msg {
        CosmosMsg::Bank(bank) => match bank {
            cosmwasm_std::BankMsg::Send { to_address, amount } => layer_std::BankMsg::Send {
                sender: sender.clone(),
                recipient: AccountId::parse_string(&to_address)?,
                amount,
            }
            .into(),
            cosmwasm_std::BankMsg::Burn { amount } => layer_std::BankMsg::Burn {
                sender: sender.clone(),
                amount,
            }
            .into(),
            x => unimplemented!("bank msg {:?}", x),
        },
        CosmosMsg::Wasm(wasm) => match wasm {
            cosmwasm_std::WasmMsg::Execute {
                contract_addr,
                msg,
                funds,
            } => layer_std::WasmMsg::Execute {
                contract_addr: AccountId::parse_string(&contract_addr)?,
                msg,
                sender: sender.clone(),
                funds,
            }
            .into(),
            cosmwasm_std::WasmMsg::Instantiate {
                admin,
                code_id,
                msg,
                funds,
                label,
            } => layer_std::WasmMsg::Instantiate {
                sender: sender.clone(),
                admin: admin.map(|x| AccountId::parse_string(&x)).transpose()?,
                code_id,
                msg,
                funds,
                label,
            }
            .into(),
            cosmwasm_std::WasmMsg::Instantiate2 {
                admin,
                code_id,
                label,
                msg,
                funds,
                salt,
            } => layer_std::WasmMsg::Instantiate2 {
                sender: sender.clone(),
                admin: admin.map(|x| AccountId::parse_string(&x)).transpose()?,
                code_id,
                msg,
                funds,
                label,
                salt,
            }
            .into(),
            cosmwasm_std::WasmMsg::Migrate {
                contract_addr,
                msg,
                new_code_id,
            } => layer_std::WasmMsg::Migrate {
                contract_addr: AccountId::parse_string(&contract_addr)?,
                msg,
                sender: sender.clone(),
                new_code_id,
            }
            .into(),
            cosmwasm_std::WasmMsg::UpdateAdmin {
                contract_addr,
                admin,
            } => layer_std::WasmMsg::UpdateAdmin {
                sender: sender.clone(),
                contract_addr: AccountId::parse_string(&contract_addr)?,
                admin: AccountId::parse_string(&admin)?,
            }
            .into(),
            cosmwasm_std::WasmMsg::ClearAdmin { contract_addr } => layer_std::WasmMsg::ClearAdmin {
                sender: sender.clone(),
                contract_addr: AccountId::parse_string(&contract_addr)?,
            }
            .into(),
            x => unimplemented!("wasm msg {:?}", x),
        },
        CosmosMsg::Custom(custom) => {
            let root = root_account();
            ensure_eq!(sender, &root, WasmError::NotRoot);
            match custom {
                CustomRootMsg::Sudo { contract_addr, msg } => layer_std::WasmMsg::Sudo {
                    sender: root,
                    contract_addr: AccountId::parse_string(&contract_addr)?,
                    msg,
                }
                .into(),
                CustomRootMsg::ClearAdmin { contract_addr } => layer_std::WasmMsg::ClearAdmin {
                    sender: root,
                    contract_addr: AccountId::parse_string(&contract_addr)?,
                }
                .into(),
                CustomRootMsg::UpdateAdmin {
                    contract_addr,
                    admin,
                } => layer_std::WasmMsg::UpdateAdmin {
                    sender: root,
                    contract_addr: AccountId::parse_string(&contract_addr)?,
                    admin: AccountId::parse_string(&admin)?,
                }
                .into(),
                CustomRootMsg::Pin { code_id } => layer_std::WasmMsg::Pin {
                    sender: root,
                    code_id,
                }
                .into(),
                CustomRootMsg::Unpin { code_id } => layer_std::WasmMsg::Unpin {
                    sender: root,
                    code_id,
                }
                .into(),
                CustomRootMsg::Migrate {
                    contract_addr,
                    new_code_id,
                    msg,
                } => layer_std::WasmMsg::Migrate {
                    sender: root,
                    contract_addr: AccountId::parse_string(&contract_addr)?,
                    new_code_id,
                    msg,
                }
                .into(),
            }
        }
        _ => todo!(),
    };
    Ok(res)
}

use cosmos_sdk_proto::traits::{Message, TypeUrl};

pub fn encode_cosmwasm_response(data: MsgData) -> (&'static str, Vec<u8>) {
    // FIXME: we used to have nice types from cosmrs...
    // cosmos_sdk_proto::cosmos::bank::v1beta1::MsgSend::TYPE_URL,
    // but they were incorrect, we needed the response type. Unfortuntely, this line doesn't work
    // cosmos_sdk_proto::cosmos::bank::v1beta1::MsgSendResponse::TYPE_URL,
    // So we just encode them manually

    match data {
        MsgData::Bank(bank) => match bank {
            BankMsgData::Send {} => (
                "/cosmos.bank.v1beta1.MsgSendResponse",
                cosmos_sdk_proto::cosmos::bank::v1beta1::MsgSendResponse {}.encode_to_vec(),
            ),
            BankMsgData::Burn {} => unknown_cosmwasm_response(),
        },
        MsgData::Wasm(wasm) => match wasm {
            WasmMsgData::Store { code_id, checksum } => (
                "/cosmwasm.wasm.v1.MsgStoreCodeResponse",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgStoreCodeResponse {
                    code_id,
                    checksum: checksum.into(),
                }
                .encode_to_vec(),
            ),
            WasmMsgData::Execute { data } => (
                "/cosmwasm.wasm.v1.MsgExecuteContractResponse",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgExecuteContractResponse {
                    data: data.to_vec(),
                }
                .encode_to_vec(),
            ),
            WasmMsgData::Instantiate { contract, data } => (
                "/cosmwasm.wasm.v1.MsgInstantiateContractResponse",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgInstantiateContractResponse {
                    address: contract.to_string(),
                    data: data.to_vec(),
                }
                .encode_to_vec(),
            ),
            WasmMsgData::Instantiate2 { contract, data } => (
                "/cosmwasm.wasm.v1.MsgInstantiateContract2Response",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgInstantiateContract2Response {
                    address: contract.to_string(),
                    data: data.to_vec(),
                }
                .encode_to_vec(),
            ),
            WasmMsgData::Migrate { data } => (
                "/cosmwasm.wasm.v1.MsgMigrateContractResponse",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgMigrateContractResponse {
                    data: data.to_vec(),
                }
                .encode_to_vec(),
            ),
            WasmMsgData::UpdateAdmin {} => (
                "/cosmwasm.wasm.v1.MsgUpdateAdminResponse",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgUpdateAdminResponse {}.encode_to_vec(),
            ),
            WasmMsgData::ClearAdmin {} => (
                "/cosmwasm.wasm.v1.MsgClearAdminResponse",
                cosmos_sdk_proto::cosmwasm::wasm::v1::MsgClearAdminResponse {}.encode_to_vec(),
            ),
            WasmMsgData::Sudo { data: _ } => unknown_cosmwasm_response(),
            WasmMsgData::PinCode {} => unknown_cosmwasm_response(),
            WasmMsgData::UnpinCode {} => unknown_cosmwasm_response(),
        },
    }
}

/// This is a helper response for encode_cosmwasm_response for those who have no
/// Cosmos SDK message corresponding to them
fn unknown_cosmwasm_response() -> (&'static str, Vec<u8>) {
    (
        cosmos_sdk_proto::cosmos::bank::v1beta1::MsgSend::TYPE_URL,
        cosmos_sdk_proto::cosmos::bank::v1beta1::MsgSendResponse {}.encode_to_vec(),
    )
}

// convert vec to binary if non-empty, else None
fn maybe_binary(data: Vec<u8>) -> Option<Binary> {
    if data.is_empty() {
        None
    } else {
        Some(data.into())
    }
}

/// This modifies the data field on the parent one, but keep the type.
/// If this MsgData doesn't have such a field, do nothing
fn set_data_field(parent_data: &mut MsgData, new_data: Binary) {
    if let MsgData::Wasm(wasm) = parent_data {
        match wasm {
            WasmMsgData::Execute { data } => *data = new_data,
            WasmMsgData::Instantiate { data, .. } => *data = new_data,
            WasmMsgData::Instantiate2 { data, .. } => *data = new_data,
            WasmMsgData::Migrate { data } => *data = new_data,
            WasmMsgData::Sudo { data } => *data = new_data,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::{coin, coins, testing::mock_env, to_json_binary, Event};
    use layer_storage::{MemoryStore, PersistentStorage};

    use crate::AppConfig;

    use super::*;

    #[test]
    fn properly_set_data_field() {
        let mut parent = MsgData::Wasm(WasmMsgData::Execute {
            data: b"initial".into(),
        });
        set_data_field(&mut parent, b"updated".into());
        assert_eq!(
            parent,
            MsgData::Wasm(WasmMsgData::Execute {
                data: b"updated".into()
            })
        );
    }

    const HACKATOM: &[u8] = include_bytes!("../../fixtures/hackatom.wasm");

    // copied from testing/utils.rs cuz issue importing
    fn event_value<'a>(events: &'a [Event], ty: &str, key: &str) -> Option<&'a str> {
        events.iter().find(|a| a.ty == ty).and_then(|evt| {
            evt.attributes
                .iter()
                .find(|a| a.key == key)
                .map(|attr| attr.value.as_str())
        })
    }

    // FIXME: similar test for instantiate, migrate (but they call same dispatch_msgs, so not essential)

    #[test]
    fn process_msg_enforces_allow_submsg() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let block = mock_env().block;
        let meter = GasMeter::new(1_000_000);
        let sm = StateMachine::new(&AppConfig::new(
            "/tmp/slay3r/process_msg_enforces_allow_submsg",
        ));

        let sender = AccountId::unchecked("sender");
        let verifier = AccountId::unchecked("verifier");
        let beneficiary = AccountId::unchecked("beneficiary");
        let init_funds = vec![coin(20, "btc"), coin(100, "eth")];

        // set money
        sm.bank
            .init_balance(&mut store, &meter, &sender, init_funds)
            .unwrap();

        // store hackatom wasm
        let msg = WasmMsg::StoreCode {
            sender: sender.clone(),
            code: HACKATOM.into(),
        };
        let resp = sm
            .wasm
            .process_msg(&mut store, &meter, &block, &sm, &sender, msg)
            .unwrap();
        let code_id: u64 = event_value(&resp.events, "store_code", "code_id")
            .unwrap()
            .parse()
            .unwrap();

        // init hackatom contract
        let init_msg = hackatom_msgs::InstantiateMsg {
            verifier: verifier.to_string(),
            beneficiary: beneficiary.to_string(),
        };
        let msg = WasmMsg::Instantiate {
            sender: sender.clone(),
            admin: None,
            code_id,
            msg: to_json_binary(&init_msg).unwrap(),
            funds: coins(45, "eth"),
            label: "Hackatom Contract".into(),
        };
        let resp = sm
            .wasm
            .process_msg(&mut store, &meter, &block, &sm, &sender, msg)
            .unwrap();
        let event = event_value(&resp.events, "instantiate", "_contract_address").unwrap();
        let contract_addr = AccountId::parse_string(event).unwrap();

        // prepare proper release
        let exec_msg = hackatom_msgs::ExecuteMsg::Release {};
        let msg = WasmMsg::Execute {
            sender: verifier.clone(),
            contract_addr,
            msg: to_json_binary(&exec_msg).unwrap(),
            funds: vec![],
        };

        // execute fails with no_submsgs set
        let err = sm
            .wasm
            .process_msg_no_submsg(&mut store, &meter, &block, &sm, &verifier, msg.clone())
            .unwrap_err();
        assert_eq!(err, PulsarError::Wasm(WasmError::SubMsgNotSupported));

        // make a normal call that uses subnmessages, succeeds
        let _resp = sm
            .wasm
            .process_msg(&mut store, &meter, &block, &sm, &verifier, msg.clone())
            .unwrap();
    }

    /// This is copied from https://github.com/CosmWasm/cosmwasm/blob/v1.2.6/contracts/hackatom/src/msg.rs
    pub mod hackatom_msgs {
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
}
