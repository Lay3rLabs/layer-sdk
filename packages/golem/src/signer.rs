use std::rc::Rc;
use std::str::FromStr;

use bip32::{DerivationPath, Mnemonic, PrivateKey, XPrv};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use layer_std::AccountId;

use super::core::MOCK_CHAIN_INFO;

#[derive(Clone, Debug)]
pub struct KeyConfig {
    pub address_prefix: String,
    pub coin_type: u32,
}

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            address_prefix: MOCK_CHAIN_INFO.network_info.pub_address_prefix.into(),
            coin_type: MOCK_CHAIN_INFO.network_info.coin_type,
        }
    }
}

#[derive(Clone)]
pub struct DerivedKey {
    mnemonic: Rc<Mnemonic>,
    config: Rc<KeyConfig>,
    index: u32,
    key: SigningKey,
}

impl DerivedKey {
    // This creates the original signing key for a given mnemonic
    pub fn new(mnemonic: String, config: KeyConfig) -> Self {
        let index = 0u32;
        let mnemonic = Mnemonic::new(mnemonic, Default::default()).unwrap();
        let key = derive_key(&mnemonic, config.coin_type, index);
        Self {
            mnemonic: Rc::new(mnemonic),
            config: Rc::new(config),
            index,
            key,
        }
    }

    // This clones the metadata and derives a new key from the mnemonic with the given index
    pub fn with_index(&self, index: u32) -> Self {
        let key = derive_key(&self.mnemonic, self.config.coin_type, index);
        Self {
            mnemonic: self.mnemonic.clone(),
            config: self.config.clone(),
            index,
            key,
        }
    }

    pub fn sign_prehash(&self, prehash: &[u8]) -> Vec<u8> {
        let sig: Signature = self.key.sign_prehash(prehash).unwrap();
        sig.to_vec()
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn pub_key(&self) -> layer_std::PubKey {
        let pk = self.key.public_key();
        let raw_point = pk.to_encoded_point(true);
        layer_std::PubKey::secp256k1(raw_point.as_bytes())
    }

    pub fn account(&self) -> AccountId {
        self.pub_key().account_id().unwrap()
    }
}

fn derive_key(mnemonic: &Mnemonic, coin_type: u32, index: u32) -> SigningKey {
    let path = format!("m/44'/{}'/0'/0/{}", coin_type, index);
    let derive = DerivationPath::from_str(&path).unwrap();
    let seed = mnemonic.to_seed("");
    let xprv = XPrv::derive_from_path(&seed, &derive).unwrap();
    xprv.private_key().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_proper_addresses() {
        let mnemonic = "economy stock theory fatal elder harbor betray wasp final emotion task crumble siren bottom lizard educate guess current outdoor pair theory focus wife stone".to_string();
        let config = KeyConfig::default();
        let key = DerivedKey::new(mnemonic, config);

        assert_eq!(
            key.account(),
            AccountId::parse_string("0x0d82b1E7c96dbfA42462fE612932e6bfF111D51B").unwrap() // AccountId::parse_string("slay3r1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmvk3r3j").unwrap()
        );
        assert_eq!(key.index(), 0);

        let key2 = key.with_index(1);
        assert_eq!(
            key2.account(),
            AccountId::parse_string("0x7b48329ca54696F0e7F97566C4d654C4658431e3").unwrap() // AccountId::parse_string("slay3r10dyr9899g6t0pelew4nvf4j5c3jcgv0rf3kguu").unwrap()
        );
        assert_eq!(key2.index(), 1);
    }
}
