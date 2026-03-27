use cosmwasm_std::Event;

use crate::GasMeter;
use crate::MsgData;

/// Response from one message, to be combined for TxResponse
#[derive(Debug)]
pub struct MsgResponse {
    pub data: MsgData,
    pub events: Vec<Event>,
}

impl MsgResponse {
    pub fn new(events: Vec<Event>, data: impl Into<MsgData>) -> Self {
        MsgResponse {
            events,
            data: data.into(),
        }
    }
}

// We get the gas_used / gas_wanted from the gas meter (outside of scope)
// Errors get codespace = "slay3r", code = 1, log = err.to_string()
// Success get data and events
// One entry in data and events per message
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxResponse {
    pub data: Vec<MsgData>,
    pub events: Vec<Vec<Event>>,
}

impl TxResponse {
    pub fn new(data: Vec<MsgData>, events: Vec<Vec<Event>>) -> Self {
        TxResponse { data, events }
    }

    pub fn empty() -> Self {
        TxResponse {
            data: vec![],
            events: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GasInfo {
    pub gas_used: u64,
    pub gas_wanted: u64,
}

impl GasInfo {
    pub fn from_meter(meter: &GasMeter) -> Self {
        GasInfo {
            gas_used: meter.used(),
            gas_wanted: meter.limit(),
        }
    }

    pub fn zero() -> Self {
        GasInfo {
            gas_used: 0,
            gas_wanted: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxResult<E: std::error::Error> {
    pub gas: GasInfo,
    pub result: Result<TxResponse, E>,
}

impl<E: std::error::Error> TxResult<E> {
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }

    pub fn is_err(&self) -> bool {
        self.result.is_err()
    }

    pub fn failure(err: E) -> Self {
        TxResult {
            gas: GasInfo::zero(),
            result: Err(err),
        }
    }
}
