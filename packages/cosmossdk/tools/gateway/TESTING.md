# Testing Full-Stack Compatibility with gRPC Gateway

First, we need to start this up, using docker compose:

```bash
./scripts/reset_volumes.sh
docker compose up
```

## Connect Keplr

Now, we need to register the devnet with Keplr

Go to [Alexar Devtools](https://docs.axelar.dev/resources/keplr#add-your-custom-network)
And replacing the JSON configuration with the one in [`devnet.json`](./devnet.json), and clicking "Validate Input & Add to Keplr". 

A Keplr Pop-Up will appear and you should approve.

Now, go to Keplr, click on "Cosmos Hub" to select the chain selector, and scroll all the way to the bottom where you see "Slay3r DevNet" down under Beta. Select this one and check your balance.

For more fun, import the test mnemonic `economy stock theory fatal elder harbor betray wasp final emotion task crumble siren bottom lizard educate guess current outdoor pair theory focus wife stone`
which you can get from [the integration tests](../integration/src/testutils.spec.ts).

(Note: sending still fails. Legacy Amino issue?)

## Getting tokens

```bash
curl --header "Content-Type: application/json" \
  --request POST \
  --data '{"denom":"uslay","address":"layer1z6cn9ahndgpz9xnzydad08qpkzlrkdjp0aym3s"}' \
  http://localhost:8000/credit
```
