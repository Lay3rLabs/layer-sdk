use tracing::debug_span;

use cosmwasm_schema::cw_serde;
use cosmwasm_std::BlockInfo;
use cw_storage_plus::KeyDeserialize;

use layer_std::{
    response::{AccountResponse, QueryResponse},
    AccountId, AuthQuery, GasMeter, Msg, PubKey, Tx, TxError,
};
use layer_storage::{prefixed, prefixed_read, Map, ReadonlyStorage, Storage};

use crate::error::{PulsarError, PulsarResult};
use crate::sm::StateMachine;

use super::AuthError;

pub const NAMESPACE_AUTH: &[u8] = b"auth";
const ACCOUNTS: Map<&AccountId, Account> = Map::new("accounts");

pub fn parse_keys(bucket: &str, key: Vec<u8>) -> Vec<String> {
    match bucket {
        "accounts" => vec![AccountId::from_vec(key).unwrap().to_string()],
        // FIXME: make robust for production, but this helps debug
        _ => panic!("unknown bucket {}", bucket),
    }
}

// Store magic accounts here
// TODO: Initialize with InternalAccount on startup
// TODO: make this some config?
pub fn fee_collector_account() -> AccountId {
    AccountId::new(&[7u8; 20]).unwrap()
}

pub const GAS_COST_SIG_VALIDATION: u64 = 1_000;

#[cw_serde]
pub enum Account {
    /// This is External Account in Ethereum terms, controlled by a public key
    External { pubkey: PubKey, sequence: u64 },
    /// No pubkey can control this, either contract or "module account"
    Internal {},
    /// Used for account abstraction, where a contract can validate what a pubkey can do
    Smart {
        // FIXME: any more info to add here?
        contract: AccountId,
    },
}

#[derive(Debug, Clone)]
pub struct Auth {}

impl Auth {
    pub fn new() -> Self {
        Auth {}
    }

    /// This checks (and bumps) sequences of the local storage and deducts the fees from the account.
    /// If successful, it returns the TxData with all info that needs to be executed.
    pub fn validate_tx(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        block: &BlockInfo,
        sm: &StateMachine,
        tx: Tx,
        // Set to false in simulate only
        validate_sig: bool,
    ) -> PulsarResult<TxData> {
        let _span = debug_span!("auth.validate_tx", validate_sig).entered();

        // later handle other types
        let Tx::Signed(tx) = tx;

        // load the signer account if any
        let mut auth_store = prefixed(storage, NAMESPACE_AUTH);
        let pubkey = match ACCOUNTS.may_load(&auth_store, meter, &tx.signer)? {
            Some(Account::External { pubkey, sequence }) => {
                // ensure sequence matches
                if validate_sig && sequence != tx.signing_info.sequence {
                    return Err(TxError::InvalidSequence {
                        provided: tx.signing_info.sequence,
                        expected: sequence,
                    }
                    .into());
                }
                // if pubkey is in signing info, it must match
                if let Some(pk) = &tx.signing_info.pubkey {
                    if pk != &pubkey {
                        return Err(TxError::PubKeyMismatch {}.into());
                    }
                }
                // store the bumped sequence
                let account = Account::External {
                    pubkey: pubkey.clone(),
                    sequence: sequence + 1,
                };
                ACCOUNTS.save(&mut auth_store, meter, &tx.signer, &account)?;
                // use the pubkey in the account to validate
                pubkey
            }
            None => {
                // if no account, ensure sequence is 0
                if validate_sig && tx.signing_info.sequence != 0 {
                    return Err(TxError::InvalidSequence {
                        provided: tx.signing_info.sequence,
                        expected: 0,
                    }
                    .into());
                }
                // ensure pubkey exists and matches account
                match &tx.signing_info.pubkey {
                    Some(pk) => {
                        if pk.account_id()? != tx.signer {
                            return Err(TxError::PubKeyMismatch.into());
                        }
                        let account = Account::External {
                            pubkey: pk.clone(),
                            // Save with sequence 1, so next tx must use that (not empty 0)
                            sequence: 1,
                        };
                        ACCOUNTS.save(&mut auth_store, meter, &tx.signer, &account)?;
                        // use the pubkey in the account to validate
                        pk.clone()
                    }
                    None => return Err(TxError::PubKeyMissing.into()),
                }
            }
            Some(Account::Internal {}) => {
                // this is always prohibited
                return Err(TxError::InternalAcccount.into());
            }
            Some(Account::Smart { .. }) => {
                todo!();
            }
        };

        // validate the signature with that account (Cosmos-specific)
        // gas cost always charges (even in simulate) to provide more accurate gas estimation
        meter.charge(GAS_COST_SIG_VALIDATION)?;
        if validate_sig {
            pubkey.validate_signature(&tx.signing_info.message_hash, &tx.signing_info.signature)?;
        }

        // TODO: filter logic on gas pricing.... charge min fee

        // charge fee info from tx sender (not pubkey if smart account)
        if let Some(fee) = tx.fee.fee {
            // Skip 0 values like None values (for cw-orch compatibility)
            if !fee.amount.is_zero() {
                sm.bank.transfer(
                    storage,
                    meter,
                    block,
                    sm,
                    tx.signer.clone(),
                    fee_collector_account(),
                    vec![fee],
                )?;
            }
        }

        // Return data
        Ok(TxData {
            signer: tx.signer,
            msgs: tx.msgs,
            gas_wanted: tx.fee.gas_limit,
        })
    }

    pub fn query(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        _block: &BlockInfo,
        _sm: &StateMachine,
        request: AuthQuery,
    ) -> PulsarResult<QueryResponse<PulsarError>> {
        let auth_storage = prefixed_read(storage, NAMESPACE_AUTH);
        match request {
            AuthQuery::Account { address } => {
                let account = ACCOUNTS.may_load(&auth_storage, meter, &address)?;
                let res = match account {
                    Some(Account::External { pubkey, sequence }) => AccountResponse::External {
                        address,
                        pubkey: Some(pubkey),
                        sequence,
                    },
                    Some(Account::Internal {}) => AccountResponse::Internal { address },
                    Some(Account::Smart { contract }) => {
                        AccountResponse::Smart { contract, address }
                    }
                    None => AccountResponse::External {
                        address,
                        pubkey: None,
                        sequence: 0,
                    },
                };
                Ok(res.into())
            }
        }
    }

    /// This is to be called from other modules (eg. wasm) to claim an address
    /// for internal use only, like a contract, but also fee collector.
    /// This ensures no pubkey can later take control of this address.
    pub fn claim_internal_account(
        &self,
        storage: &mut dyn Storage,
        meter: &GasMeter,
        address: &AccountId,
    ) -> PulsarResult<()> {
        let mut auth_store = prefixed(storage, NAMESPACE_AUTH);
        if ACCOUNTS
            .may_load(auth_store.as_ref(), meter, address)?
            .is_some()
        {
            return Err(AuthError::AccountExists(address.clone()).into());
        }
        let account = Account::Internal {};
        ACCOUNTS.save(&mut auth_store, meter, address, &account)?;
        Ok(())
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self::new()
    }
}

// info on a validated transaction
pub struct TxData {
    /// We only support one sender per transaction
    pub signer: AccountId,

    /// All messages in the payload, decoded to Slay3r format
    pub msgs: Vec<Msg>,

    /// Amount of gas this transaction may use
    pub gas_wanted: u64,
}
