# Faster ABCI

This is intended to be a faster implementation of the ABCI Server, compared to 
[Tendermint ABCI](https://github.com/informalsystems/tendermint-rs/tree/main/abci).
The previous implementation used 1 thread per connection that called the application inline.

This implementation uses tokio async for all network connections, and identifies which connection
has which purpose (query, check, deliver, snapshot).  Each connection is then handled by a separate
rayon threadpool, with adjustable size. `deliver`, which gets `finalize_block`,
`prepare_proposal` and `process_proposal` must always have a size of 1.
However, this allows us to have a large number of `check` and `query` connections in parallel.

Note, we assume the actual ABCI application is thread-safe and normal sync / blocking code.
It will run in Rayon. The purpose of async is to handle the various connections.