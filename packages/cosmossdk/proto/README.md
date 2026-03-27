# Proto definitions

This directory is Rust server code generated from `/proto/**` files by running the program in `/tools/proto-compiler`.

It is meant to be imported to run the grpc server (also enabling the "server" feature).

## Demos

It also contains some simple example code to test the sync server.
If you run a local setup (`docker compose up`), you can run the following to test state sync:

```bash
cargo run --example sync_test --features=client
```
