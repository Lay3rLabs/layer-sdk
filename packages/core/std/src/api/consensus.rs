// based on https://github.com/informalsystems/tendermint-rs/blob/mikhail/cometbft-0.38/proto/src/prost/v0_38/tendermint.types.rs
// but let's make this more generic

use cosmwasm_schema::cw_serde;

use crate::Duration;

const DEFAULT_APP_VERSION: u64 = 1;
const DEFAULT_BLOCK_SIZE: u64 = 2 * 1024 * 1024;
pub const DEFAULT_BLOCK_GAS: u64 = 20_000_000;

const DEFAULT_EVIDENCE_AGE: u64 = 21 * 86400;

// This is tendermint control stuff... we may need to abstract this in the future
// Removed many of those items we likely don't change
#[derive(Debug, Clone, PartialEq)]
pub struct ConsensusParams {
    pub block: BlockParams,
    pub evidence: EvidenceParams,
    pub version: u64,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        ConsensusParams {
            block: Default::default(),
            evidence: Default::default(),
            version: DEFAULT_APP_VERSION,
        }
    }
}

#[cw_serde]
pub struct BlockParams {
    pub max_bytes: u64,
    /// None means no limit
    pub max_gas: Option<u64>,
}

impl Default for BlockParams {
    fn default() -> Self {
        BlockParams {
            max_bytes: DEFAULT_BLOCK_SIZE,
            max_gas: Some(DEFAULT_BLOCK_GAS),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceParams {
    pub max_age_blocks: u64,
    pub max_age_time: Duration,
    pub max_bytes: u64,
}

impl Default for EvidenceParams {
    fn default() -> Self {
        EvidenceParams {
            max_age_blocks: DEFAULT_EVIDENCE_AGE / 5,
            max_age_time: Duration::from_seconds(DEFAULT_EVIDENCE_AGE),
            max_bytes: 1024 * 1024,
        }
    }
}
