use std::fs::create_dir_all;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{signer::KeyConfig, DerivedKey, Slay3rGolem};

const DEFAULT_MNEMONIC: &str = "economy stock theory fatal elder harbor betray wasp final emotion task crumble siren bottom lizard educate guess current outdoor pair theory focus wife stone";

const DEFAULT_TMP_DIR_BASE: &str = "slay3r-golem";

#[derive(Clone, Debug, Default)]
pub struct Slay3rGolemBuilder {
    // set to a default
    mnemonic: Option<String>,
    // make a random temp-dir
    cache_dir: Option<String>,
    // set to a default
    key_config: KeyConfig,
}

impl Slay3rGolemBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    // Finds a path from the cache_dir
    pub fn ensure_cache_dir(&self) -> Result<String, std::io::Error> {
        if let Some(dir) = self.cache_dir.as_ref() {
            let path = Path::new(dir);
            // it is doesn't exist, make it
            if !path.try_exists()? {
                create_dir_all(path)?;
            }
            Ok(dir.to_owned())
        } else {
            // let _ = std::fs::remove_dir_all(path);
            // std::fs::create_dir_all(path).unwrap();
            // path
            let mut tmp_dir = std::env::temp_dir();
            tmp_dir.push(DEFAULT_TMP_DIR_BASE);
            tmp_dir.push(format!("test-{}", timestamp_nanos()));
            println!("{}", tmp_dir.display());
            create_dir_all(&tmp_dir)?;
            let path = tmp_dir.into_os_string().into_string().unwrap();
            Ok(path)
        }
    }

    pub fn build(self) -> Slay3rGolem {
        let cache_dir = self.ensure_cache_dir().unwrap();
        let mnemonic = self
            .mnemonic
            .unwrap_or_else(|| DEFAULT_MNEMONIC.to_string());
        let signer = DerivedKey::new(mnemonic, self.key_config);
        Slay3rGolem::new(&cache_dir, signer)
    }
}

// This should be good enough for unique number for dirs
fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    use cosmwasm_std::coins;
    use cw_orch_core::environment::{BankQuerier, DefaultQueriers, TxHandler};

    #[test]
    fn test_builder() {
        let builder = Slay3rGolemBuilder::new();
        let chain = builder.build();
        assert_eq!(chain.signer.index(), 0);

        // check we initialized balances properly
        let bank_query = chain.bank_querier();
        let balance = bank_query.balance(chain.sender_addr(), None).unwrap();
        assert_eq!(balance, coins(2_000_000_000u128, "uslay"));

        // all further tests are in Slay3rTube... just check setup here
    }
}
