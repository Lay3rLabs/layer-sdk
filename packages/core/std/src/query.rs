use cosmwasm_std::{Binary, Coin, StdError};
use derivative::Derivative;
use std::error::Error as Err;
use std::fmt::{Display, Formatter};
use thiserror::Error;

use crate::account_id::{AccountId, AccountIdError};
use crate::api::TxResult;
use crate::tx::Tx;

// 2.0: use cosmwasm_std::Checksum
type Checksum = Binary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Return the raw binary value stored under that key
    Raw {
        key: Vec<u8>,
    },
    Auth(AuthQuery),
    Bank(BankQuery),
    Simulate(Tx),
    Wasm(WasmQuery),
}

impl From<AuthQuery> for Query {
    fn from(value: AuthQuery) -> Self {
        Query::Auth(value)
    }
}

impl From<BankQuery> for Query {
    fn from(value: BankQuery) -> Self {
        Query::Bank(value)
    }
}

impl From<WasmQuery> for Query {
    fn from(value: WasmQuery) -> Self {
        Query::Wasm(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankQuery {
    /// Returns the supply of all native tokens
    /// Return value is of type TotalSupplyResponse.
    TotalSupply {},
    /// Return value is of type SupplyResponse.
    Supply { denom: String },
    /// Return value is BalanceResponse
    Balance { address: AccountId, denom: String },
    /// Note that this may be much more expensive than Balance and should be avoided if possible.
    /// Return value is AllBalanceResponse.
    AllBalances { address: AccountId },
}

impl Display for BankQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BankQuery::TotalSupply { .. } => f.write_str("BankQuery::TotalSupply"),
            BankQuery::Supply { .. } => f.write_str("BankQuery::Supply"),
            BankQuery::Balance { .. } => f.write_str("BankQuery::Balance"),
            BankQuery::AllBalances { .. } => f.write_str("BankQuery::AllBalances"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthQuery {
    /// Return value is of type AccountResponse.
    Account { address: AccountId },
}

impl Display for AuthQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthQuery::Account { .. } => f.write_str("AuthQuery::Account"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResponse<E: Err> {
    Raw { key: Vec<u8>, value: Vec<u8> },
    Auth(AuthQueryResponse),
    Bank(BankQueryResponse),
    Simulate(TxResult<E>),
    Wasm(WasmQueryResponse),
}

impl<E: Err> From<AuthQueryResponse> for QueryResponse<E> {
    fn from(value: AuthQueryResponse) -> Self {
        QueryResponse::Auth(value)
    }
}

impl<E: Err> From<BankQueryResponse> for QueryResponse<E> {
    fn from(value: BankQueryResponse) -> Self {
        QueryResponse::Bank(value)
    }
}

impl<E: Err> From<WasmQueryResponse> for QueryResponse<E> {
    fn from(value: WasmQueryResponse) -> Self {
        QueryResponse::Wasm(value)
    }
}

impl<E: Err> From<TxResult<E>> for QueryResponse<E> {
    fn from(value: TxResult<E>) -> Self {
        QueryResponse::Simulate(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthQueryResponse {
    Account(AccountResponse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountResponse {
    /// This is External Account in Ethereum terms, controlled by a public key
    External {
        address: AccountId,
        pubkey: Option<crate::PubKey>,
        sequence: u64,
    },
    /// No pubkey can control this, either contract or "module account"
    Internal { address: AccountId },
    /// Used for account abstraction, where a contract can validate what a pubkey can do
    Smart {
        address: AccountId,
        // FIXME: any more info to add here?
        contract: AccountId,
    },
}

impl AccountResponse {
    pub fn address(&self) -> &AccountId {
        match self {
            AccountResponse::External { address, .. } => address,
            AccountResponse::Internal { address } => address,
            AccountResponse::Smart { address, .. } => address,
        }
    }

    /// Gets sequence if an external account, panics otherwise
    pub fn external_sequence(&self) -> u64 {
        match self {
            AccountResponse::External { sequence, .. } => *sequence,
            AccountResponse::Internal { .. } => panic!("internal account has no sequence"),
            AccountResponse::Smart { .. } => panic!("smart account has no sequence"),
        }
    }

    /// Returns true iff this is an external account
    pub fn is_external(&self) -> bool {
        matches!(self, AccountResponse::External { .. })
    }
}

impl<E: Err> From<AccountResponse> for QueryResponse<E> {
    fn from(value: AccountResponse) -> Self {
        AuthQueryResponse::Account(value).into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankQueryResponse {
    TotalSupply(TotalSupplyResponse),
    Supply(SupplyResponse),
    Balance(BalanceResponse),
    AllBalances(AllBalanceResponse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotalSupplyResponse {
    pub amounts: Vec<Coin>,
}

impl<E: Err> From<TotalSupplyResponse> for QueryResponse<E> {
    fn from(value: TotalSupplyResponse) -> Self {
        BankQueryResponse::TotalSupply(value).into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupplyResponse {
    /// Always returns a Coin with the requested denom.
    /// This will be of zero amount if the denom does not exist.
    pub amount: Coin,
}

impl<E: Err> From<SupplyResponse> for QueryResponse<E> {
    fn from(value: SupplyResponse) -> Self {
        BankQueryResponse::Supply(value).into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceResponse {
    /// Always returns a Coin with the requested denom.
    /// This may be of 0 amount if no such funds.
    pub amount: Coin,
}

impl<E: Err> From<BalanceResponse> for QueryResponse<E> {
    fn from(value: BalanceResponse) -> Self {
        BankQueryResponse::Balance(value).into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllBalanceResponse {
    /// Returns all non-zero coins held by this account.
    pub amount: Vec<Coin>,
}

impl<E: Err> From<AllBalanceResponse> for QueryResponse<E> {
    fn from(value: AllBalanceResponse) -> Self {
        BankQueryResponse::AllBalances(value).into()
    }
}

#[derive(Derivative, Debug, Clone, PartialEq, Eq)]
pub enum WasmQuery {
    /// this queries the public API of another contract at a known address (with known ABI)
    /// Return value is whatever the contract returns (caller should know), wrapped in a
    /// ContractResult that is JSON encoded.
    Smart {
        contract_addr: AccountId,
        /// msg is the json-encoded QueryMsg struct
        #[derivative(Debug(format_with = "layer_std::binary_to_string"))]
        msg: Binary,
    },
    /// this queries the raw kv-store of the contract.
    /// returns the raw, unparsed data stored at that key, which may be an empty vector if not present
    Raw {
        contract_addr: AccountId,
        /// Key is the raw key used in the contracts Storage
        key: Binary,
    },
    /// Returns a [`ContractInfoResponse`] with metadata on the contract from the runtime
    ContractInfo { contract_addr: AccountId },
    /// Returns a [`CodeInfoResponse`] with metadata of the code
    CodeInfo { code_id: u64, include_wasm: bool },
    /// Returns a [`ListCodesResponse`] with metadata of the codes, starting from the given code_id
    ListCodes {
        from: Option<u64>,
        limit: Option<u32>,
    },
    /// Returns a [`ContractsByCodeResponse`] with list of addresses using this code_id
    ContractsByCode { code_id: u64 },
}

#[derive(Derivative, Debug, Clone, PartialEq, Eq)]
pub enum WasmQueryResponse {
    Smart(#[derivative(Debug(format_with = "layer_std::binary_to_string"))] Binary),
    Raw(#[derivative(Debug(format_with = "layer_std::binary_to_string"))] Binary),
    ContractInfo(ContractInfoResponse),
    CodeInfo(CodeInfoResponse),
    ListCodes(ListCodesResponse),
    ContractsByCode(ContractsByCodeResponse),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractInfoResponse {
    /// The contract address queried
    pub addresss: AccountId,
    /// The code id of the contract
    pub code_id: u64,
    /// address that instantiated this contract
    pub creator: AccountId,
    /// admin who can run migrations (if any)
    pub admin: Option<AccountId>,
    /// human-readable label for this contract (optional)
    pub label: String,
    /// if set, the contract is pinned to the cache, and thus uses less gas when called
    pub pinned: bool,
    /// set if this contract has bound an IBC port
    pub ibc_port: Option<String>,

    /// blockchain height when contract was first created
    pub created: u64,
}

impl From<ContractInfoResponse> for cosmwasm_std::ContractInfoResponse {
    fn from(value: ContractInfoResponse) -> Self {
        let mut res = cosmwasm_std::ContractInfoResponse::default();
        res.code_id = value.code_id;
        res.creator = value.creator.to_string();
        res.admin = value.admin.map(|a| a.to_string());
        res.pinned = value.pinned;
        res.ibc_port = value.ibc_port;
        res
    }
}

/// The essential data from wasmd's [CodeInfo]/[CodeInfoResponse].
///
/// `code_hash`/`data_hash` was renamed to `checksum` to follow the CosmWasm
/// convention and naming in `instantiate2_address`.
///
/// [CodeInfo]: https://github.com/CosmWasm/wasmd/blob/v0.30.0/proto/cosmwasm/wasm/v1/types.proto#L62-L72
/// [CodeInfoResponse]: https://github.com/CosmWasm/wasmd/blob/v0.30.0/proto/cosmwasm/wasm/v1/query.proto#L184-L199
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeInfoResponse {
    pub code_info: CodeInfo,
    // The actual WASM code blob
    pub data: Binary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeInfo {
    pub code_id: u64,
    /// The address that initially stored the code
    pub creator: AccountId,
    /// The hash of the Wasm blob (aka data_hash)
    pub checksum: Checksum,
    /// If this code is pinned to the cache
    pub pinned: bool,
    // TODO: instantiate permissions... if we choose to use them
}

/// The essential data from wasmd's [CodeInfo]/[CodeInfoResponse].
///
/// `code_hash`/`data_hash` was renamed to `checksum` to follow the CosmWasm
/// convention and naming in `instantiate2_address`.
///
/// [QueryCodesResponse]: https://github.com/CosmWasm/wasmd/blob/v0.30.0/proto/cosmwasm/wasm/v1/query.proto#L215-L220
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCodesResponse {
    pub code_infos: Vec<CodeInfo>,
}

/// The essential data from wasmd's [CodeInfo]/[CodeInfoResponse].
///
/// `code_hash`/`data_hash` was renamed to `checksum` to follow the CosmWasm
/// convention and naming in `instantiate2_address`.
///
/// [QueryContractsByCodeResponse](https://github.com/CosmWasm/wasmd/blob/v0.30.0/proto/cosmwasm/wasm/v1/query.proto#L121-L129)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractsByCodeResponse {
    /// All contract addresses currently using this code_id
    pub contracts: Vec<AccountId>,
}

#[derive(Error, Debug, PartialEq)]
pub enum QueryError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("Unsupported path: {0}")]
    UnsupportedPath(String),

    #[error("{0}")]
    Addr(#[from] AccountIdError),

    /// FIXME: either ensure all callers of this function produce determinstic strings,
    /// Or remove all info
    #[error("Parse: {0}")]
    ParseError(String),

    /// FIXME: either ensure all callers of this function produce determinstic strings,
    /// Or remove all info
    #[error("Encoding: {0}")]
    EncodingError(String),
}
