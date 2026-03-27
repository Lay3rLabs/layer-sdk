use bytes::Bytes;
use cosmwasm_std::{Binary, Coin};
use derivative::Derivative;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::account_id::AccountId;
use crate::msg::{Msg, MsgError};

use crate::pubkey::PubKey;

/// A list of various tx formats we accept.
/// We start with Cosmos-SDK format for compatibility, but want to later allow native signing format.
/// We can pass this around to auth to allow handling multiple types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tx {
    /// Cosmos Format. Note that we support a subset of the functionality:
    /// Only one signer, no authz or fee grants. But that means 90%+ of tx work, and are "Keplr compatible"
    Signed(SignedTx),
}

impl Tx {
    pub fn tx_len(&self) -> u64 {
        match self {
            Tx::Signed(s) => s.tx_len(),
        }
    }

    pub fn tx_hash(&self) -> Vec<u8> {
        match self {
            Tx::Signed(s) => s.tx_hash(),
        }
    }
}

/// This is a struct to represent a parsed transaction.
/// The encoding schemes are defined separately and there are many ways to transform
/// raw bytes into a proper SignedTx instance.
/// Once of which is the Cosmos SDK format (direct or legacy amino signing modes)
/// There will be others more native to Slay3r in the future, or for compatibility with other chains.
#[derive(Derivative, Clone, PartialEq, Eq)]
#[derivative(Debug)]
pub struct SignedTx {
    // The decoded messages inside this transaction
    pub msgs: Vec<Msg>,

    // The account that will execute them (must match the pubkey in signing_info if that is set)
    pub signer: AccountId,

    /// this is info on the signer (pubkey, sequence)
    pub signing_info: SigningInfo,

    /// this is info on the fee (amount and gas wanted)
    pub fee: FeeInfo,

    // TODO: implement this
    /// if set and the chain height is greater than this, abort the tx in all cases
    pub timeout_height: Option<u64>,

    // The original transaction bytes (generally not needed, except to calculate the length for gas)
    #[derivative(Debug = "ignore")]
    pub raw_tx: Bytes,
}

impl SignedTx {
    pub fn tx_len(&self) -> u64 {
        self.raw_tx.len() as u64
    }

    pub fn tx_hash(&self) -> Vec<u8> {
        Sha256::digest(&self.raw_tx).to_vec()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningInfo {
    /// These are the raw bytes that should be properly signed by a pubkey to be valid.
    /// It depends fully on the raw encoding of the transaction.
    /// It is a hash of the sign bytes that can be fed into a public key verification function
    pub message_hash: Binary,

    /// This is the sequence number from the given pubkey -
    pub sequence: u64,

    /// This is the pubkey used to sign. If None, the signer address has previously
    /// signed transactions on this chain, and we can lookup the pubkey from the chain state.
    pub pubkey: Option<PubKey>,

    /// These are raw signature bytes that make sense based on the pubkey type
    pub signature: Binary,
}

impl SigningInfo {
    pub fn validate_signature(&self) -> Result<(), TxError> {
        match &self.pubkey {
            Some(pk) => pk.validate_signature(&self.message_hash, &self.signature),
            None => Err(TxError::MissingPubKey),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeeInfo {
    // how much they pay
    pub fee: Option<Coin>,
    // how much work to do
    pub gas_limit: u64,
}

#[derive(Error, Debug, PartialEq)]
pub enum TxError {
    #[error("{0}")]
    Msg(#[from] MsgError),

    #[error("No signer on the tx")]
    NoSigner,

    #[error("More than one signer on this tx")]
    MultipleSigners,

    #[error("Trying to pay fees in multiple denoms, we allow only 1")]
    MultipleFeeDenoms,

    #[error("No support for complex pubkey types: {0}")]
    UnsupportedPubKey(&'static str),

    #[error("No support for signing mode: {0}")]
    UnsupportedSigningMode(&'static str),

    #[error("Tx extensions present but not supported")]
    ExtensionsNotSupported,

    #[error("Fee grant field used but not supported")]
    FeeGrantNotSupported,

    #[error("Fee payer field used but not supported")]
    FeePayerNotSupported,

    #[error("No public key provided to validate the signature")]
    MissingPubKey,

    #[error("The signature doesn't match the claimed pubkey and the message hash")]
    InvalidSignature,

    #[error("The provided sequence {provided} doesn't match, expected {expected}")]
    InvalidSequence { provided: u64, expected: u64 },

    #[error("PubKey provided in the tx doesn't match the pubkey of the sender account")]
    PubKeyMismatch,

    #[error("The first transaction provided by an account must contain the pubkey")]
    PubKeyMissing,

    #[error("Cannot execute an external transaction from an internal account")]
    InternalAcccount,

    /// FIXME: either ensure all callers of this function produce determinstic strings,
    /// Or remove all info
    #[error("Parse: {0}")]
    ParseError(String),
}
