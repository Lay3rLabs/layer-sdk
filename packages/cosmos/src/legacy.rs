use cosmwasm_std::{Binary, StdError};
use serde::Serialize;
use serde_json::value::Value;

use layer_std::{BankMsg, FeeInfo, Msg, WasmMsg};

// We must sort the keys alphabetically to get the "amino serialization"
#[derive(Serialize, Debug)]
pub struct StdSignDoc {
    pub account_number: String,
    pub chain_id: String,
    pub fee: StdFee,
    pub memo: String,
    pub msgs: Vec<AminoMsg>,
    pub sequence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_height: Option<String>,
}

impl StdSignDoc {
    pub fn build(
        doc: cosmrs::tx::SignDoc,
        msgs: &[Msg],
        fee: &FeeInfo,
        sequence: u64,
        memo: &str,
    ) -> Self {
        let msgs = msgs.iter().map(AminoMsg::build).collect();
        StdSignDoc {
            account_number: doc.account_number.to_string(),
            chain_id: doc.chain_id,
            fee: fee.into(),
            memo: memo.into(), // Do we skip serialization if empty string?
            msgs,
            sequence: sequence.to_string(),
            timeout_height: None,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, StdError> {
        // Serialize to "sorted" JSON
        let raw =
            serde_json::to_string(self).map_err(|e| StdError::serialize_err("StdSignDoc", e))?;
        // Escape special characters as per amino implementation
        let escaped = raw
            .replace('&', "\\u0026")
            .replace('<', "\\u003c")
            .replace('>', "\\u003e");
        // Turn into bytes
        Ok(escaped.into_bytes())
    }
}

#[derive(Serialize, Debug)]
pub struct StdFee {
    pub amount: Vec<Coin>,
    pub gas: String,
    // Unsupported for now
    // /** The granter address that is used for paying with feegrants */
    // #[serde(skip_serializing_if = "Option::is_none")]
    // pub granter: Option<String>,
    // /** The fee payer address. The payer must have signed the transaction. */
    // #[serde(skip_serializing_if = "Option::is_none")]
    // pub payer: Option<String>,
}

impl From<&layer_std::FeeInfo> for StdFee {
    fn from(value: &layer_std::FeeInfo) -> Self {
        Self {
            amount: value.fee.iter().map(Into::into).collect(),
            gas: value.gas_limit.to_string(),
        }
    }
}

#[derive(Serialize, Debug)]
pub struct Coin {
    pub amount: String,
    pub denom: String,
}

impl From<&cosmrs::Coin> for Coin {
    fn from(value: &cosmrs::Coin) -> Self {
        Coin {
            amount: value.amount.to_string(),
            denom: value.denom.to_string(),
        }
    }
}

impl From<&cosmwasm_std::Coin> for Coin {
    fn from(value: &cosmwasm_std::Coin) -> Self {
        Coin {
            amount: value.amount.to_string(),
            denom: value.denom.to_string(),
        }
    }
}

/// Reference definitions from CosmJS encoder
/// https://github.com/cosmos/cosmjs/blob/main/packages/cosmwasm-stargate/src/modules/wasm/aminomessages.ts
/// https://github.com/cosmos/cosmjs/blob/main/packages/stargate/src/modules/bank/aminomessages.ts
#[derive(Serialize, Debug)]
#[serde(tag = "type", content = "value")]
pub enum AminoMsg {
    #[serde(rename = "cosmos-sdk/MsgSend")]
    MsgSend(AminoMsgSend),
    #[serde(rename = "wasm/MsgExecuteContract")]
    MsgExecute(AminoMsgExecute),
    #[serde(rename = "wasm/MsgInstantiateContract")]
    MsgInstantiate(AminoMsgInstantiate),
    // TODO
    // #[serde(rename = "wasm/MsgInstantiateContract2")]
    // MsgInstantiate2(AminoMsgInstantiate2),
    // #[serde(rename = "wasm/MsgMigrateContract")]
    // MsgMigrate(AminoMsgMigrate),
    // #[serde(rename = "wasm/MsgUpdateAdmin")]
    // MsgUpdateAdmin(AminoMsgUpdateAdmin),
    // #[serde(rename = "wasm/MsgClearAdmin")]
    // MsgClearAdmin(AminoMsgClearAdmin),
    #[serde(rename = "bogus/shit")]
    Other(()),
}

impl AminoMsg {
    pub fn build(msg: &Msg) -> Self {
        match msg {
            Msg::Bank(BankMsg::Send {
                sender,
                recipient,
                amount,
            }) => AminoMsg::MsgSend(AminoMsgSend {
                amount: amount.iter().map(Into::into).collect(),
                from_address: sender.to_string(),
                to_address: recipient.to_string(),
            }),
            Msg::Wasm(WasmMsg::Execute {
                contract_addr,
                msg,
                sender,
                funds,
            }) => AminoMsg::MsgExecute(AminoMsgExecute {
                funds: funds.iter().map(Into::into).collect(),
                sender: sender.to_string(),
                contract: contract_addr.to_string(),
                msg: convert_message(msg),
            }),
            Msg::Wasm(WasmMsg::Instantiate {
                msg,
                sender,
                funds,
                admin,
                code_id,
                label,
            }) => AminoMsg::MsgInstantiate(AminoMsgInstantiate {
                funds: funds.iter().map(Into::into).collect(),
                sender: sender.to_string(),
                msg: convert_message(msg),
                admin: admin.as_ref().map(|a| a.to_string()),
                code_id: code_id.to_string(),
                label: label.to_string(),
            }),
            _ => AminoMsg::Other(()),
        }
    }
}

// This "magically" creates recursively sorted JSON object when reserialized
fn convert_message(msg: &Binary) -> Value {
    serde_json::from_slice(msg).unwrap()
}

#[derive(Serialize, Debug)]
pub struct AminoMsgSend {
    pub amount: Vec<Coin>,
    pub from_address: String,
    pub to_address: String,
}

#[derive(Serialize, Debug)]
pub struct AminoMsgExecute {
    pub contract: String,
    pub funds: Vec<Coin>,
    pub msg: Value,
    pub sender: String,
}

#[derive(Serialize, Debug)]
pub struct AminoMsgInstantiate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<String>,
    pub code_id: String,
    pub funds: Vec<Coin>,
    pub label: String,
    pub msg: Value,
    pub sender: String,
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::Coin;
    use layer_std::AccountId;
    use serde::Deserialize;

    use super::*;

    #[derive(Serialize)]
    pub struct DemoMsg {
        pub name: String,
        pub age: u32,
        pub height: Option<u32>,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    pub struct EmbeddedMessage {
        pub name: String,
        pub age: u32,
        pub items: Vec<SomeItem>,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    pub struct SomeItem {
        pub zeta: u32,
        pub alpha: u32,
    }

    #[test]
    fn try_manual_sorting() {
        let orig_msg = EmbeddedMessage {
            name: "John Smith".into(),
            age: 32,
            items: vec![
                SomeItem { zeta: 25, alpha: 3 },
                SomeItem { zeta: 10, alpha: 1 },
            ],
        };
        let msg = serde_json::to_vec(&orig_msg).unwrap();
        let val: Value = serde_json::from_slice(&msg).unwrap();
        let reserialized = serde_json::to_string(&val).unwrap();

        // make sure this is properly sorted
        let expected = r#"{"age":32,"items":[{"alpha":3,"zeta":25},{"alpha":1,"zeta":10}],"name":"John Smith"}"#;
        assert_eq!(reserialized.as_str(), expected);

        // ensure it parses back to the original array
        let parsed: EmbeddedMessage = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(orig_msg, parsed);
    }

    #[test]
    fn check_convert_message() {
        let orig_msg = EmbeddedMessage {
            name: "John Smith".into(),
            age: 32,
            items: vec![
                SomeItem { zeta: 25, alpha: 3 },
                SomeItem { zeta: 10, alpha: 1 },
            ],
        };
        let msg: Binary = serde_json::to_vec(&orig_msg).unwrap().into();
        let raw = convert_message(&msg);
        let reserialized = serde_json::to_string(&raw).unwrap();
        let expected = r#"{"age":32,"items":[{"alpha":3,"zeta":25},{"alpha":1,"zeta":10}],"name":"John Smith"}"#;
        assert_eq!(reserialized.as_str(), expected);
    }

    #[test]
    fn check_convert_execute() {
        let orig_msg = DemoMsg {
            name: "John Smith".into(),
            age: 32,
            height: Some(187),
        };
        // cargo run slay3r1ve6ku6mevd5xjcmtv4hqqqqqqqqqqqqqc5nacv slay3r1vfkxzcmtdphkcetndahqqqqqqqqqqqqqyet8zv
        //
        //
        let exec_msg = Msg::Wasm(WasmMsg::Execute {
            sender: AccountId::parse_string("0x66756e6b79636869636b656E0000000000000000").unwrap(),
            contract_addr: AccountId::parse_string("0x626C61636b686F6c65736f6E0000000000000000")
                .unwrap(),
            msg: serde_json::to_vec(&orig_msg).unwrap().into(),
            funds: vec![],
        });

        let amino_msg = AminoMsg::build(&exec_msg);
        let output = serde_json::to_string(&amino_msg).unwrap();
        let expected = r#"{"type":"wasm/MsgExecuteContract","value":{"contract":"0x626C61636b686F6c65736f6E0000000000000000","funds":[],"msg":{"age":32,"height":187,"name":"John Smith"},"sender":"0x66756e6b79636869636b656E0000000000000000"}}"#;
        assert_eq!(output, expected);
    }

    #[test]
    fn check_convert_instantiate_with_fund_admin() {
        let orig_msg = DemoMsg {
            name: "n00b".into(),
            age: 18,
            height: Some(165),
        };
        let init_msg = Msg::Wasm(WasmMsg::Instantiate {
            sender: AccountId::parse_string("0x66756e6b79636869636b656E0000000000000000").unwrap(),
            admin: Some(
                AccountId::parse_string("0x626C61636b686F6c65736f6E0000000000000000").unwrap(),
            ),
            code_id: 12345,
            label: "sticky".into(),
            msg: serde_json::to_vec(&orig_msg).unwrap().into(),
            funds: vec![Coin::new(1234u128, "uslay")],
        });

        let amino_msg = AminoMsg::build(&init_msg);
        let output = serde_json::to_string(&amino_msg).unwrap();
        let expected = r#"{"type":"wasm/MsgInstantiateContract","value":{"admin":"0x626C61636b686F6c65736f6E0000000000000000","code_id":"12345","funds":[{"amount":"1234","denom":"uslay"}],"label":"sticky","msg":{"age":18,"height":165,"name":"n00b"},"sender":"0x66756e6b79636869636b656E0000000000000000"}}"#;
        assert_eq!(output, expected);
    }

    // Ensure empty admin is not serialized but empty fund are
    #[test]
    fn check_convert_instantiate_no_fund_admin() {
        let orig_msg = DemoMsg {
            name: "n00b".into(),
            age: 18,
            height: Some(165),
        };
        let init_msg = Msg::Wasm(WasmMsg::Instantiate {
            sender: AccountId::parse_string("0x66756e6b79636869636b656E0000000000000000").unwrap(),
            admin: None,
            code_id: 12345,
            label: "sticky".into(),
            msg: serde_json::to_vec(&orig_msg).unwrap().into(),
            funds: vec![],
        });

        let amino_msg = AminoMsg::build(&init_msg);
        let output = serde_json::to_string(&amino_msg).unwrap();
        let expected = r#"{"type":"wasm/MsgInstantiateContract","value":{"code_id":"12345","funds":[],"label":"sticky","msg":{"age":18,"height":165,"name":"n00b"},"sender":"0x66756e6b79636869636b656E0000000000000000"}}"#;
        assert_eq!(output, expected);
    }
}
