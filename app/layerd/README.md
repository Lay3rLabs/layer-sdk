# Slay3r Daemon

This is the core application, and ABCI-enabled app that talks with tendermint to convert
the powerful, pure-Rust state machine into a full-fledged blockchain.

## Configuration

All values can be specified either in a `slay3r.toml` file (located in `~/.slay3r/config`),
via environmental variables (prefixed with `SLAY_`), or as command-line flags.
The actions listed later take precedence over those listed earlier.

Any field not defined will use defaults from the code.

You can see this via:

```shell
SLAY_LOG="debug" cargo run -- --host 0.0.0.0
```

To see the full options:

```shell
cargo run -- -h
```

## Running with CometBFT

### Install Software

You can install version `v0.38.0-alpha.2` of CometBFT by following the [installation instructions](https://github.com/cometbft/cometbft/blob/v0.38.0-alpha.2/docs/guides/install.md)

```shell
mkdir -p ~/golang
cd ~/golang
git clone https://github.com/cometbft/cometbft.git
cd cometbft
git checkout v0.38.0-alpha.2
make install

cometbft version
ls -l $(which cometbft)
# Around 26MB at v0.38.0-alpha.2!
```

You also need to install the slay3rd binary from this repo:

```shell
cargo install --path ./app/slay3rd

slay3rd -h
ls -l $(which slay3rd)
# Around 2.7MB at v0.2.0!
```

(Dev note: checking size and optimization...)

```shell
cd ./app/slay3rd
RUSTFLAGS='-C link-arg=-s' cargo build --release
ls -l ../../target/release/slay3rd
# Around 1.8MB at v0.2.0!
```

### Setup Configuration

Set up basic tendermint

```
rm -rf ~/.slay3r
mkdir -p ~/.slay3r
cometbft init --home ~/.slay3r
```


Add the following to `~/.slay3r/config/genesis.json`:

```json
  "app_state": {
    "bank": [
      {
        "address": "layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug",
        "balance": [
          {
            "amount": "4000000000",
            "denom": "uslay"
          }
        ]
      }
    ],
    "wasm": {
      "gov_account": "layer1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmt53rug"
    }
  }
```

### First execution

One terminal:

```
cometbft start --home ~/.slay3r
```

Second terminal:

```
slay3rd --log debug
```

### Reset

Since we have a memory store for the app, once we stop it is out of sync with tendermint.
We need to reset and restart. (We will be able to continue later with similar version
once we have a disk store.)

```shell
cometbft unsafe-reset-all --home ~/.slay3r
```

### Dev Mode

For debugging, let's run a quick build of `slay3rd`:

```shell
cd ./app/slay3rd
SLAY_LOG=debug,tendermint_abci::application=error cargo run
```

For cometbft, we also need to reset state every crash.

```
cometbft unsafe-reset-all --home ~/.slay3r
cometbft start --home ~/.slay3r
```


Then just keep restarting the `cometbft` and `slay3rd` process on crash

Update: you also need to run the gateway

```bash
cd ./gateway
go run main.go -grpc-server-endpoint localhost:9090
```

(Maybe with docker?)

```bash
docker run --network host ghcr.io/lay3rlabs/gateway:latest /app -grpc-server-endpoint localhost:9090
```

/cosmos.base.tendermint.v1beta1.Service/GetNodeInfo

/cosmos.tx.v1beta1.Service/Simulate (Broadcast)