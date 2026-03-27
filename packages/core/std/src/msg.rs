use cosmwasm_std::{Binary, Coin, StdError};
use derivative::Derivative;
use itertools::Itertools;
use std::fmt::{Display, Formatter};
use thiserror::Error;

use crate::account_id::{AccountId, AccountIdError};

// 2.0: use cosmwasm_std::Checksum
type Checksum = Binary;

/// This is the internal message format used in Slay3r.
/// We convert various wire formats into this before processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Bank(BankMsg),
    Wasm(WasmMsg),
}

impl From<BankMsg> for Msg {
    fn from(value: BankMsg) -> Self {
        Msg::Bank(value)
    }
}

impl From<WasmMsg> for Msg {
    fn from(value: WasmMsg) -> Self {
        Msg::Wasm(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankMsg {
    Send {
        sender: AccountId,
        recipient: AccountId,
        amount: Vec<Coin>,
    },
    Burn {
        sender: AccountId,
        amount: Vec<Coin>,
    },
}

impl Display for BankMsg {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BankMsg::Send { .. } => f.write_str("BankMsg::Send"),
            BankMsg::Burn { .. } => f.write_str("BankMsg::Burn"),
        }
    }
}

#[derive(Derivative, Clone, PartialEq, Eq)]
#[derivative(Debug)]
pub enum WasmMsg {
    StoreCode {
        sender: AccountId,
        #[derivative(Debug(format_with = "crate::wasm_summary"))]
        code: Binary,
    },
    /// Dispatches a call to another contract at a known address (with known ABI).
    ///
    /// This is translated to a [MsgExecuteContract](https://github.com/CosmWasm/wasmd/blob/v0.14.0/x/wasm/internal/types/tx.proto#L68-L78).
    /// `sender` is automatically filled with the current contract's address.
    Execute {
        sender: AccountId,
        contract_addr: AccountId,
        /// msg is the json-encoded ExecuteMsg struct (as raw Binary)
        #[derivative(Debug(format_with = "crate::binary_to_string"))]
        msg: Binary,
        funds: Vec<Coin>,
    },
    /// Instantiates a new contracts from previously uploaded Wasm code.
    ///
    /// The contract address is non-predictable. But it is guaranteed that
    /// when emitting the same Instantiate message multiple times,
    /// multiple instances on different addresses will be generated. See also
    /// Instantiate2.
    ///
    /// This is translated to a [MsgInstantiateContract](https://github.com/CosmWasm/wasmd/blob/v0.29.2/proto/cosmwasm/wasm/v1/tx.proto#L53-L71).
    /// `sender` is automatically filled with the current contract's address.
    Instantiate {
        sender: AccountId,
        admin: Option<AccountId>,
        code_id: u64,
        /// msg is the JSON-encoded InstantiateMsg struct (as raw Binary)
        #[derivative(Debug(format_with = "crate::binary_to_string"))]
        msg: Binary,
        funds: Vec<Coin>,
        /// A human-readbale label for the contract
        label: String,
    },
    /// Instantiates a new contracts from previously uploaded Wasm code
    /// using a predictable address derivation algorithm implemented in
    /// [`cosmwasm_std::instantiate2_address`].
    ///
    /// This is translated to a [MsgInstantiateContract2](https://github.com/CosmWasm/wasmd/blob/v0.29.2/proto/cosmwasm/wasm/v1/tx.proto#L73-L96).
    /// `sender` is automatically filled with the current contract's address.
    /// `fix_msg` is automatically set to false.
    Instantiate2 {
        sender: AccountId,
        admin: Option<AccountId>,
        code_id: u64,
        /// A human-readbale label for the contract
        label: String,
        /// msg is the JSON-encoded InstantiateMsg struct (as raw Binary)
        #[derivative(Debug(format_with = "crate::binary_to_string"))]
        msg: Binary,
        funds: Vec<Coin>,
        salt: Binary,
    },
    /// Migrates a given contracts to use new wasm code. Passes a MigrateMsg to allow us to
    /// customize behavior.
    ///
    /// Only the contract admin (as defined in wasmd), if any, is able to make this call.
    Migrate {
        sender: AccountId,
        contract_addr: AccountId,
        /// the code_id of the new logic to place in the given contract
        new_code_id: u64,
        /// msg is the json-encoded MigrateMsg struct that will be passed to the new code
        #[derivative(Debug(format_with = "crate::binary_to_string"))]
        msg: Binary,
    },
    /// Sets a new admin (for migrate) on the given contract.
    /// Fails if this contract is not currently admin of the target contract.
    UpdateAdmin {
        sender: AccountId,
        contract_addr: AccountId,
        admin: AccountId,
    },
    /// Clears the admin on the given contract, so no more migration possible.
    /// Fails if this contract is not currently admin of the target contract.
    ClearAdmin {
        sender: AccountId,
        contract_addr: AccountId,
    },
    /// Only the root address can call sudo.
    /// Once slay3r checks the permissions, it should be trusted as root by the contract.
    Sudo {
        sender: AccountId,
        contract_addr: AccountId,
        /// msg is the json-encoded SudoMsg struct that will be passed to the new code
        #[derivative(Debug(format_with = "crate::binary_to_string"))]
        msg: Binary,
    },
    Pin {
        sender: AccountId,
        code_id: u64,
    },
    Unpin {
        sender: AccountId,
        code_id: u64,
    },
}

impl Display for WasmMsg {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WasmMsg::Execute { .. } => f.write_str("WasmMsg::Execute"),
            WasmMsg::Instantiate { .. } => f.write_str("WasmMsg::Instantiate"),
            WasmMsg::Instantiate2 { .. } => f.write_str("WasmMsg::Instantiate2"),
            WasmMsg::Migrate { .. } => f.write_str("WasmMsg::Migrate"),
            WasmMsg::Sudo { .. } => f.write_str("WasmMsg::Sudo"),
            WasmMsg::ClearAdmin { .. } => f.write_str("WasmMsg::ClearAdmin"),
            WasmMsg::UpdateAdmin { .. } => f.write_str("WasmMsg::UpdateAdmin"),
            WasmMsg::StoreCode { .. } => f.write_str("WasmMsg::StoreCode"),
            WasmMsg::Pin { .. } => f.write_str("WasmMsg::Pin"),
            WasmMsg::Unpin { .. } => f.write_str("WasmMsg::Unpin"),
        }
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum MsgError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("Unsupported Any type: {0}")]
    UnsupportedAnyType(String),

    #[error("Tx doesn't have any messages")]
    NoMessages,

    #[error("Tx requires signatures from multiple addresses - not supported")]
    MultipleSigners,

    #[error("{0}")]
    Addr(#[from] AccountIdError),

    /// FIXME: either ensure all callers of this function produce determinstic strings,
    /// Or remove all info
    #[error("Parse: {0}")]
    ParseError(String),
}

impl Msg {
    /// List which addresses must sign the message for it to be valid
    pub fn required_signer(&self) -> AccountId {
        match &self {
            Msg::Bank(bank) => match bank {
                BankMsg::Send { sender, .. } => sender.clone(),
                BankMsg::Burn { sender, .. } => sender.clone(),
            },
            Msg::Wasm(wasm) => match wasm {
                WasmMsg::Execute { sender, .. } => sender.clone(),
                WasmMsg::Instantiate { sender, .. } => sender.clone(),
                WasmMsg::Instantiate2 { sender, .. } => sender.clone(),
                WasmMsg::Migrate { sender, .. } => sender.clone(),
                WasmMsg::Sudo { sender, .. } => sender.clone(),
                WasmMsg::UpdateAdmin { sender, .. } => sender.clone(),
                WasmMsg::ClearAdmin { sender, .. } => sender.clone(),
                WasmMsg::StoreCode { sender, .. } => sender.clone(),
                WasmMsg::Pin { sender, .. } => sender.clone(),
                WasmMsg::Unpin { sender, .. } => sender.clone(),
            },
        }
    }
}

/// Returns the signer needed by all Messages.
/// If there are no messages, or different signers required by messages, returns an error
pub fn required_signer(msgs: &[Msg]) -> Result<AccountId, MsgError> {
    let mut signers: Vec<_> = msgs.iter().map(Msg::required_signer).dedup().collect();
    if signers.len() > 1 {
        return Err(MsgError::MultipleSigners);
    }
    signers.pop().ok_or(MsgError::NoMessages)
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum MsgData {
    Wasm(WasmMsgData),
    Bank(BankMsgData),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum WasmMsgData {
    Store { code_id: u64, checksum: Checksum },
    Execute { data: Binary },
    Instantiate { contract: AccountId, data: Binary },
    Instantiate2 { contract: AccountId, data: Binary },
    Migrate { data: Binary },
    Sudo { data: Binary },
    UpdateAdmin {},
    ClearAdmin {},
    PinCode {},
    UnpinCode {},
}

impl From<WasmMsgData> for MsgData {
    fn from(value: WasmMsgData) -> Self {
        MsgData::Wasm(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankMsgData {
    Send {},
    Burn {},
}

impl From<BankMsgData> for MsgData {
    fn from(value: BankMsgData) -> Self {
        MsgData::Bank(value)
    }
}
