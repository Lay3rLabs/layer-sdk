use std::collections::HashMap;

use cosmwasm_std::Addr;

use cw_orch_core::environment::StateInterface;
use cw_orch_core::CwEnvError;

#[derive(Clone)]
pub struct OrchRegistry {
    /// Deployed contract code ids
    pub code_ids: HashMap<String, u64>,
    /// Deployed contract addresses
    pub addresses: HashMap<String, Addr>,
    /// Chain id of the mocked chain
    pub chain_id: String,
}

impl OrchRegistry {
    pub fn new(chain_id: &str) -> Self {
        Self {
            code_ids: HashMap::new(),
            addresses: HashMap::new(),
            chain_id: chain_id.to_string(),
        }
    }
}

impl Default for OrchRegistry {
    fn default() -> Self {
        Self::new("slay3r-orch")
    }
}

impl StateInterface for OrchRegistry {
    /// Get the address of a contract using the specified contract id.
    fn get_address(&self, contract_id: &str) -> Result<Addr, CwEnvError> {
        self.addresses
            .get(contract_id)
            .ok_or_else(|| CwEnvError::AddrNotInStore(contract_id.to_owned()))
            .map(|val| val.to_owned())
    }

    /// Set the address of a contract using the specified contract id.
    fn set_address(&mut self, contract_id: &str, address: &Addr) {
        self.addresses
            .insert(contract_id.to_string(), address.to_owned());
    }

    /// Get the code id for a contract with the specified contract id.
    fn get_code_id(&self, contract_id: &str) -> Result<u64, CwEnvError> {
        self.code_ids
            .get(contract_id)
            .ok_or_else(|| CwEnvError::CodeIdNotInStore(contract_id.to_owned()))
            .map(|val| val.to_owned())
    }

    /// Set the code id for a contract with the specified contract id.
    fn set_code_id(&mut self, contract_id: &str, code_id: u64) {
        self.code_ids.insert(contract_id.to_string(), code_id);
    }

    /// Get all addresses related to this deployment.
    fn get_all_addresses(&self) -> Result<std::collections::HashMap<String, Addr>, CwEnvError> {
        Ok(self.addresses.clone())
    }

    /// Get all codes related to this deployment.
    fn get_all_code_ids(&self) -> Result<std::collections::HashMap<String, u64>, CwEnvError> {
        Ok(self.code_ids.clone())
    }
}
