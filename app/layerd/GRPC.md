# Testing GRPC

Run the whole setup including gateway with:

`docker compose up`

Now, check the following endpoints:

```bash
curl localhost:1317/cosmos/bank/v1beta1/balances/layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug | jq .

curl localhost:1317/cosmos/bank/v1beta1/balances/layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug/by_denom?denom=uslay | jq .
```

```bash
curl localhost:1317/cosmos/auth/v1beta1/accounts/layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug | jq .
```

Run the integration tests and check again, along with the following cosmwasm ones:

```bash
# Is this right? Should it be snake case?
curl localhost:1317/cosmwasm/wasm/v1/code | jq .code_infos

# Warning, this dumps entire wasm blob now
curl localhost:1317/cosmwasm/wasm/v1/code/1 | jq .

# Get actual contracts
curl localhost:1317/cosmwasm/wasm/v1/code/1/contracts | jq .contracts

# Replace with address from your contract
curl localhost:1317/cosmwasm/wasm/v1/contract/layer13xthx4g4vjyp43gnwxk0mw4zeuvp96ljhh7q0pec9znj409er0csj40elw | jq .
```

```bash
echo -n '{"balance":{"address":"layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug"}}' | base64 -w0

curl localhost:1317/cosmwasm/wasm/v1/contract/layer13xthx4g4vjyp43gnwxk0mw4zeuvp96ljhh7q0pec9znj409er0csj40elw/smart/eyJiYWxhbmNlIjp7ImFkZHJlc3MiOiJsYXllcjFwa3B0cmU3ZmRrbDZnZnJ6bGVzamp2aHhobGMzcjRnbXQ1M3J1ZyJ9fQ== | jq -r .data | base64 -d
```

This is a basic end-to-end that they are working.

## Tendermint queries

Test that basic tendermint node queries are working:

```bash
# Sample cometbft endpoints
curl localhost:1317/cosmos/base/tendermint/v1beta1/syncing
curl localhost:1317/cosmos/base/tendermint/v1beta1/node_info | jq .
curl localhost:1317/cosmos/base/tendermint/v1beta1/blocks/latest
curl localhost:1317/cosmos/base/tendermint/v1beta1/blocks/123 | jq .

# Note, this should have a subfield `tx: []` rather than omitting when empty
curl localhost:1317/cosmos/base/tendermint/v1beta1/blocks/latest | jq .block.data

# Bank supply
curl localhost:1317/cosmos/bank/v1beta1/supply | jq .
curl localhost:1317/cosmos/bank/v1beta1/supply/uslay | jq .

curl localhost:26657/status | jq .result.sync_info

# Example for rpc error code
curl localhost:1317/cosmos/tx/v1beta1/decode/amino
```
