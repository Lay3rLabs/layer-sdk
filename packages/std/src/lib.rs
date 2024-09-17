mod account_id;
pub mod api;
mod encode;
mod gas;
mod msg;
mod pubkey;
mod query;
pub mod root;
mod time;
mod tx;
mod utils;

pub use account_id::{must_id, AccountId, AccountIdError, VALID_ADDR_LENGTH};
use cosmwasm_std::Binary;
pub use encode::{CoinEncode, HexEncode};
pub use gas::{GasError, GasMeter, GasResult};
pub use msg::{
    required_signer, BankMsg, BankMsgData, Msg, MsgData, MsgError, WasmMsg, WasmMsgData,
};
pub use pubkey::PubKey;
pub use query::{AuthQuery, BankQuery, Query, QueryError, WasmQuery};
pub use time::{format_timestamp_rfc3339, Duration, Rfc3339, Timestamp};
pub use tx::{FeeInfo, SignedTx, SigningInfo, Tx, TxError};
pub use utils::{string_account_or_hex, stringify_or_hex};

pub mod response {
    pub use crate::query::{
        AccountResponse, AllBalanceResponse, AuthQueryResponse, BalanceResponse, BankQueryResponse,
        CodeInfo, CodeInfoResponse, ContractInfoResponse, ContractsByCodeResponse,
        ListCodesResponse, QueryResponse, SupplyResponse, TotalSupplyResponse, WasmQueryResponse,
    };
}

pub fn binary_to_string(
    data: &Binary,
    fmt: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    match std::str::from_utf8(data.as_slice()) {
        Ok(s) => fmt.write_str(s),
        Err(_) => write!(fmt, "{:?}", data),
    }
}

pub fn wasm_summary(data: &Binary, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
    write!(fmt, "WasmBytes({})", data.len())
}
