use cosmos_sdk_proto::cosmos::base::v1beta1::Coin as SdkCoin;
use cosmwasm_std::{Coin, Uint128};

use crate::error::CosmosError;

pub(crate) fn parse_sdk_coin(coin: &SdkCoin) -> Result<Coin, CosmosError> {
    Ok(Coin {
        denom: coin.denom.clone(),
        amount: Uint128::try_from(coin.amount.as_str())?,
    })
}

pub(crate) fn parse_sdk_coins(coins: &[SdkCoin]) -> Result<Vec<Coin>, CosmosError> {
    coins.iter().map(parse_sdk_coin).collect()
}

pub(crate) fn encode_sdk_coin(coin: &Coin) -> SdkCoin {
    SdkCoin {
        denom: coin.denom.clone(),
        amount: coin.amount.to_string(),
    }
}

pub(crate) fn encode_sdk_coins(coins: &[Coin]) -> Vec<SdkCoin> {
    coins.iter().map(encode_sdk_coin).collect()
}
