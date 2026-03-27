use cosmos_sdk_proto::{
    cosmos::bank::v1beta1::MsgSend,
    cosmwasm::wasm::v1::{
        MsgClearAdmin, MsgExecuteContract, MsgInstantiateContract, MsgInstantiateContract2,
        MsgMigrateContract, MsgStoreCode, MsgUpdateAdmin,
    },
    prost::Message,
    traits::{MessageExt, TypeUrl},
};
use cosmrs::Any;
use tracing::trace_span;

use layer_std::{AccountId, BankMsg, Msg, MsgError, WasmMsg};

use crate::error::CosmosError;
use crate::unzip::unzip_if_needed;
use crate::utils::parse_sdk_coins;

const MAX_WASM_SIZE: usize = 1024 * 1024 * 2; // 2 MB

pub fn parse_cosmos_msg(msg: &Any) -> Result<Msg, MsgError> {
    let _span = trace_span!("parse_cosmos_msg").entered();
    match msg.type_url.as_str() {
        MsgSend::TYPE_URL => {
            let parsed = MsgSend::from_any(msg).map_err(CosmosError::from)?;
            Ok(BankMsg::Send {
                sender: AccountId::parse_string(&parsed.from_address)?,
                recipient: AccountId::parse_string(&parsed.to_address)?,
                amount: parse_sdk_coins(&parsed.amount)?,
            }
            .into())
        }
        MsgExecuteContract::TYPE_URL => {
            let parsed = MsgExecuteContract::from_any(msg).map_err(CosmosError::from)?;
            Ok(WasmMsg::Execute {
                sender: AccountId::parse_string(&parsed.sender)?,
                contract_addr: AccountId::parse_string(&parsed.contract)?,
                msg: parsed.msg.into(),
                funds: parse_sdk_coins(&parsed.funds)?,
            }
            .into())
        }
        MsgInstantiateContract::TYPE_URL => {
            let parsed = MsgInstantiateContract::from_any(msg).map_err(CosmosError::from)?;
            let admin = if parsed.admin.is_empty() {
                None
            } else {
                Some(AccountId::parse_string(&parsed.admin)?)
            };
            Ok(WasmMsg::Instantiate {
                sender: AccountId::parse_string(&parsed.sender)?,
                admin,
                code_id: parsed.code_id,
                msg: parsed.msg.into(),
                funds: parse_sdk_coins(&parsed.funds)?,
                label: parsed.label,
            }
            .into())
        }
        // FIXME: add this to cosmrs
        // MsgInstantiateContract2::TYPE_URL => {
        "/cosmwasm.wasm.v1.MsgInstantiateContract2" => {
            let parsed = MsgInstantiateContract2::decode(&*msg.value).map_err(CosmosError::from)?;
            let admin = if parsed.admin.is_empty() {
                None
            } else {
                Some(AccountId::parse_string(&parsed.admin)?)
            };
            Ok(WasmMsg::Instantiate2 {
                sender: AccountId::parse_string(&parsed.sender)?,
                admin,
                code_id: parsed.code_id,
                msg: parsed.msg.into(),
                funds: parse_sdk_coins(&parsed.funds)?,
                label: parsed.label,
                salt: parsed.salt.into(),
            }
            .into())
        }
        MsgStoreCode::TYPE_URL => {
            let parsed = MsgStoreCode::from_any(msg).map_err(CosmosError::from)?;
            let code = unzip_if_needed(parsed.wasm_byte_code, MAX_WASM_SIZE)?.into();
            let sender = AccountId::parse_string(&parsed.sender)?;
            Ok(WasmMsg::StoreCode { sender, code }.into())
        }
        MsgMigrateContract::TYPE_URL => {
            let parsed = MsgMigrateContract::from_any(msg).map_err(CosmosError::from)?;
            Ok(WasmMsg::Migrate {
                sender: AccountId::parse_string(&parsed.sender)?,
                contract_addr: AccountId::parse_string(&parsed.contract)?,
                msg: parsed.msg.into(),
                new_code_id: parsed.code_id,
            }
            .into())
        }
        MsgUpdateAdmin::TYPE_URL => {
            let parsed = MsgUpdateAdmin::from_any(msg).map_err(CosmosError::from)?;
            Ok(WasmMsg::UpdateAdmin {
                sender: AccountId::parse_string(&parsed.sender)?,
                contract_addr: AccountId::parse_string(&parsed.contract)?,
                admin: AccountId::parse_string(&parsed.new_admin)?,
            }
            .into())
        }
        MsgClearAdmin::TYPE_URL => {
            let parsed = MsgClearAdmin::from_any(msg).map_err(CosmosError::from)?;
            Ok(WasmMsg::ClearAdmin {
                sender: AccountId::parse_string(&parsed.sender)?,
                contract_addr: AccountId::parse_string(&parsed.contract)?,
            }
            .into())
        }

        _ => Err(MsgError::UnsupportedAnyType(msg.type_url.clone())),
    }
}
