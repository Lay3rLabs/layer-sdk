use cosmwasm_std::{Coin, StdError};

use cw_orch_core::environment::{BankQuerier, Querier};

use layer_std::{
    response::{BankQueryResponse, QueryResponse},
    AccountId, BankQuery,
};

use crate::Slay3rGolem;

pub struct Slay3rBank {
    golem: Slay3rGolem,
}

impl Slay3rBank {
    pub fn new(golem: &Slay3rGolem) -> Self {
        Self {
            golem: golem.clone(),
        }
    }
}

impl Querier for Slay3rBank {
    type Error = layer_app::PulsarError;
}

impl BankQuerier for Slay3rBank {
    /// If denom is None, returns all balances
    fn balance(
        &self,
        address: impl Into<String>,
        denom: Option<String>,
    ) -> Result<Vec<Coin>, Self::Error> {
        let app = self.golem.app.borrow();
        let address = AccountId::parse_string(&address.into())?;

        if let Some(denom) = denom {
            let query = BankQuery::Balance { address, denom };
            let res = app.query(query.into())?;
            match res {
                QueryResponse::Bank(BankQueryResponse::Balance(r)) => Ok(vec![r.amount]),
                _ => Err(StdError::generic_err("unexpected response").into()),
            }
        } else {
            let query = BankQuery::AllBalances { address };
            let res = app.query(query.into())?;
            match res {
                QueryResponse::Bank(BankQueryResponse::AllBalances(r)) => Ok(r.amount),
                _ => Err(StdError::generic_err("unexpected response").into()),
            }
        }
    }

    fn total_supply(&self) -> Result<Vec<Coin>, Self::Error> {
        let app = self.golem.app.borrow();
        let query = BankQuery::TotalSupply {};
        let res = app.query(query.into())?;
        match res {
            QueryResponse::Bank(BankQueryResponse::TotalSupply(r)) => Ok(r.amounts),
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }

    fn supply_of(&self, denom: impl Into<String>) -> Result<Coin, Self::Error> {
        let app = self.golem.app.borrow();
        let query = BankQuery::Supply {
            denom: denom.into(),
        };
        let res = app.query(query.into())?;
        match res {
            QueryResponse::Bank(BankQueryResponse::Supply(r)) => Ok(r.amount),
            _ => Err(StdError::generic_err("unexpected response").into()),
        }
    }
}
