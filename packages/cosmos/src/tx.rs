use bytes::Bytes;
use cosmos_sdk_proto::cosmos::tx::signing::v1beta1::SignMode;
use cosmos_sdk_proto::cosmos::tx::v1beta1::TxRaw;
use cosmos_sdk_proto::prost::Message;

use cosmrs::tx::SignDoc;
use cosmwasm_std::{coin, Binary, Coin};
use sha2::{Digest, Sha256};
use tracing::trace_span;

use layer_std::{required_signer, Msg};
use layer_std::{FeeInfo, SignedTx, SigningInfo, TxError};

use crate::legacy::StdSignDoc;
use crate::{parse_cosmos_msg, parse_cosmos_pubkey, CosmosError};

pub const FIXED_ACCOUNT_NUMBER: u64 = 17;

// This is parsed from cosmrs::Raw and cosmrs::Tx
/// Parses the raw cosmos tx encoding and calculate the expected sign bytes.
/// Extracts all useful info from the Tx in a simpler format for us
pub fn parse_cosmos_tx(bytes: Bytes, chain_id: &str) -> Result<layer_std::Tx, TxError> {
    let _span = trace_span!("parse_cosmos_tx").entered();
    let (tx, hashable) = parse_raw_tx(&bytes, chain_id)?;

    let msgs: Result<Vec<_>, _> = tx.body.messages.iter().map(parse_cosmos_msg).collect();
    let msgs = msgs?;
    let signer = required_signer(&msgs)?;

    // other needed info
    let fee = get_fee(&tx)?;
    let signing_info = get_signing_info(&tx, hashable, &msgs, &fee, &tx.body.memo)?;
    let timeout_height = match tx.body.timeout_height.value() {
        0 => None,
        v => Some(v),
    };

    // validate other fields not used from body
    if !tx.body.extension_options.is_empty() {
        return Err(TxError::ExtensionsNotSupported);
    }

    let tx = SignedTx {
        signer,
        msgs,
        signing_info,
        fee,
        timeout_height,
        raw_tx: bytes,
    };
    Ok(layer_std::Tx::Signed(tx))
}

struct HashableMessage {
    doc: SignDoc,
}

impl HashableMessage {
    fn hash_direct_mode(self) -> Result<Binary, CosmosError> {
        let sign_bytes = self.doc.into_bytes()?;
        let message_hash = Sha256::digest(sign_bytes).to_vec();
        Ok(message_hash.into())
    }

    fn hash_legacy_mode(
        self,
        msgs: &[Msg],
        fee: &FeeInfo,
        sequence: u64,
        memo: &str,
    ) -> Result<Binary, CosmosError> {
        let sign_bytes = StdSignDoc::build(self.doc, msgs, fee, sequence, memo).to_bytes()?;
        let message_hash = Sha256::digest(&sign_bytes).to_vec();
        println!("Amino: {}", String::from_utf8(sign_bytes).unwrap());
        Ok(message_hash.into())
    }
}

fn parse_raw_tx(
    bytes: &[u8],
    chain_id: &str,
) -> Result<(cosmrs::Tx, HashableMessage), CosmosError> {
    // get raw format for accurate signing info (to validate sig)
    let raw = TxRaw::decode(bytes)?;
    // FIXME: add tx hash here as well from TxRaw?
    // FIXME: we need to do this in some match statement - only works for direct mode
    let doc = SignDoc {
        body_bytes: raw.body_bytes,
        auth_info_bytes: raw.auth_info_bytes,
        chain_id: chain_id.to_string(),
        account_number: FIXED_ACCOUNT_NUMBER,
    };
    let hashable = HashableMessage { doc };

    // parse into cosmrs::Tx so we can understand what we have
    let span = trace_span!("cosmrs::Tx::from_bytes").entered();
    let tx = cosmrs::Tx::from_bytes(bytes)?;
    span.exit();

    Ok((tx, hashable))
}

fn get_signing_info(
    tx: &cosmrs::Tx,
    hashable: HashableMessage,
    msgs: &[Msg],
    fee: &FeeInfo,
    memo: &str,
) -> Result<SigningInfo, TxError> {
    let sigs = &tx.signatures;
    let signature = match sigs.len() {
        0 => Err(TxError::NoSigner),
        1 => Ok(Binary::from(sigs[0].as_slice())),
        _ => Err(TxError::MultipleSigners),
    }?;

    let infos = &tx.auth_info.signer_infos;
    let info = match infos.len() {
        0 => Err(TxError::NoSigner),
        1 => Ok(&infos[0]),
        _ => Err(TxError::MultipleSigners),
    }?;

    let sequence = info.sequence;
    let pubkey = info
        .public_key
        .as_ref()
        .map(parse_cosmos_pubkey)
        .transpose()?;

    // assert we have sign-mode-direct (need to add legacy amino support later)
    let message_hash = match info.mode_info {
        cosmrs::tx::mode_info::ModeInfo::Single(s) => match s.mode {
            // FIXME: some better way of handling this?? SIGN_MODE_UNSPECIFIED should only be used for simulate
            // For now, we treat it like direct
            SignMode::Direct | SignMode::Unspecified => Ok(hashable.hash_direct_mode()?),
            SignMode::LegacyAminoJson => Ok(hashable.hash_legacy_mode(msgs, fee, sequence, memo)?),
            m => Err(TxError::UnsupportedSigningMode(m.as_str_name())),
        },
        _ => Err(TxError::UnsupportedSigningMode("multi")),
    }?;

    Ok(SigningInfo {
        message_hash,
        sequence,
        pubkey,
        signature,
    })
}

fn parse_fee_coin(fee_coin: &cosmrs::Coin) -> Coin {
    coin(fee_coin.amount, fee_coin.denom.as_ref())
}

pub fn get_fee(tx: &cosmrs::Tx) -> Result<FeeInfo, TxError> {
    let info = &tx.auth_info.fee;
    let fee = match info.amount.len() {
        0 => None,
        1 => Some(parse_fee_coin(&info.amount[0])),
        _ => {
            return Err(TxError::MultipleFeeDenoms);
        }
    };
    let gas_limit = info.gas_limit;

    // assert some fields empty
    if info.granter.is_some() {
        return Err(TxError::FeeGrantNotSupported);
    }
    if info.payer.is_some() {
        return Err(TxError::FeePayerNotSupported);
    }

    Ok(FeeInfo { fee, gas_limit })
}

#[cfg(test)]
mod test {
    use super::*;

    use cosmrs::{
        bank::MsgSend,
        crypto::secp256k1,
        tx::{self, Fee, Msg, SignDoc, SignerInfo},
        Coin,
    };

    use layer_std::{AccountId, BankMsg, PubKey};

    const DEFAULT_BECH32_PREFIX: &str = "slay3r";

    // TODO: this is broken as cosmrs seems to require bech32 - we can otherwise encode the id, but the MsgSend creation below
    // requires the bech32 objects from cosmrs. Anyway around this? Or alternatives to cosmrs?
    #[test]
    fn happy_path_tx_parsing() {
        let sender_private_key = secp256k1::SigningKey::random();
        let sender_public_key = sender_private_key.public_key();
        let sender_account_id = sender_public_key.account_id(DEFAULT_BECH32_PREFIX).unwrap();

        let rcpt_account_id = secp256k1::SigningKey::random()
            .public_key()
            .account_id(DEFAULT_BECH32_PREFIX)
            .unwrap();

        let sequence_number = 5;
        let chain_id = "tslay3r-1".parse().unwrap();
        let gas = 350_000u64;
        let timeout_height = 9001u16;

        let amount = Coin {
            amount: 1_000_000u128,
            denom: "uatom".parse().unwrap(),
        };
        let fee = Coin {
            amount: 200_000u128,
            denom: "uatom".parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: sender_account_id.clone(),
            to_address: rcpt_account_id.clone(),
            amount: vec![amount],
        };

        let tx_body = tx::Body::new(vec![msg_send.to_any().unwrap()], "", timeout_height);
        let signer_info = SignerInfo::single_direct(Some(sender_public_key), sequence_number);
        let auth_info = signer_info.auth_info(Fee::from_amount_and_gas(fee, gas));

        // The "sign doc" contains a message to be signed.
        let sign_doc = SignDoc::new(&tx_body, &auth_info, &chain_id, FIXED_ACCOUNT_NUMBER).unwrap();
        // Sign the "sign doc" with the sender's private key, producing a signed raw transaction.
        let tx_signed = sign_doc.sign(&sender_private_key).unwrap();
        // Serialize the raw transaction as bytes (i.e. `Vec<u8>`).
        let tx_bytes = tx_signed.to_bytes().unwrap();

        // now let's parse and see if we have the proper values
        let tx = parse_cosmos_tx(tx_bytes.into(), chain_id.as_str()).unwrap();

        // validate we have the expected values
        let layer_std::Tx::Signed(tx) = tx;
        assert_eq!(tx.timeout_height, Some(timeout_height as u64));
        assert_eq!(tx.fee.fee, Some(cosmwasm_std::coin(200_000u128, "uatom")));
        assert_eq!(tx.fee.gas_limit, gas);
        assert_eq!(tx.msgs.len(), 1);

        let sender_addr = AccountId::parse_string(sender_account_id.as_ref()).unwrap();
        let rcpt_addr = AccountId::parse_string(rcpt_account_id.as_ref()).unwrap();
        match &tx.msgs[0] {
            layer_std::Msg::Bank(BankMsg::Send {
                sender,
                amount,
                recipient,
            }) => {
                assert_eq!(sender, &sender_addr);
                assert_eq!(recipient, &rcpt_addr);
                assert_eq!(amount, &[cosmwasm_std::coin(1_000_000u128, "uatom")]);
            }
            _ => panic!("incorrect message"),
        };
        assert_eq!(tx.signer, sender_addr);

        assert_eq!(tx.signing_info.sequence, sequence_number);
        assert!(tx.signing_info.pubkey.is_some());

        // basic signature checks
        assert_eq!(tx.signing_info.signature.len(), 64);
        let Some(PubKey::Secp256k1(pk)) = &tx.signing_info.pubkey else {
            panic!("Wrong pubkey type")
        };
        assert_eq!(pk.len(), 33);

        // validate
        tx.signing_info.validate_signature().unwrap();
    }

    #[test]
    fn happy_legacy_tx_signing() {
        // TODO: find some test vectors (or generate them from cosmjs)
        assert_eq!(1, 1)
    }
}
