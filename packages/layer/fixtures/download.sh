#!/bin/bash

# Add source of any fixtures here, so we can bump versions if needed
## Get the abstract one as well to test large sizes
FILES=(
https://github.com/CosmWasm/cosmwasm/releases/download/v1.4.1/hackatom.wasm
https://github.com/CosmWasm/cw-plus/releases/download/v1.1.1/cw20_base.wasm
https://github.com/AbstractSDK/abstract/raw/v0.19.2/framework/artifacts/abstract_manager.wasm
)

for file in ${FILES[@]}; do
  echo "Downloading $file"
  curl -L -s -O "$file"
done

# compress cw20 for testing

gzip -k -f cw20_base.wasm
