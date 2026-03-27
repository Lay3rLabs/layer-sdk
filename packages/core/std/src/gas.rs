use std::cell::RefCell;

use thiserror::Error;
use tracing::trace;

/// Tracks gas usage and returns error when it hits the limit
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GasMeter(RefCell<GasCounter>);

impl GasMeter {
    pub fn new(limit: u64) -> Self {
        Self(RefCell::new(GasCounter::new(limit)))
    }

    pub fn infinite() -> Self {
        Self(RefCell::new(GasCounter::infinite()))
    }

    pub fn used(&self) -> u64 {
        self.0.borrow().used()
    }

    pub fn limit(&self) -> u64 {
        self.0.borrow().limit()
    }

    pub fn remaining(&self) -> u64 {
        self.0.borrow().remaining()
    }

    pub fn charge(&self, cost: u64) -> Result<(), GasError> {
        self.0.borrow_mut().charge(cost)
    }
}

/// Tracks gas usage and returns error when it hits the limit
#[derive(Debug, Clone, PartialEq, Eq)]
struct GasCounter {
    limit: u64,
    used: u64,
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum GasError {
    #[error("Out of gas")]
    OutOfGas,
}

pub type GasResult<T> = Result<T, GasError>;

impl GasCounter {
    fn new(limit: u64) -> Self {
        GasCounter { limit, used: 0 }
    }

    fn infinite() -> Self {
        GasCounter::new(u64::MAX)
    }

    fn used(&self) -> u64 {
        self.used
    }

    fn limit(&self) -> u64 {
        self.limit
    }

    fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    fn charge(&mut self, cost: u64) -> Result<(), GasError> {
        trace!(cost, "charge gas");
        self.used += cost;
        if self.used >= self.limit {
            Err(GasError::OutOfGas)
        } else {
            Ok(())
        }
    }
}
