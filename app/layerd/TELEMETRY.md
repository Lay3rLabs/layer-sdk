# Telemetry

How to collect great data via [OpenTelemetry](https://crates.io/crates/opentelemetry)

## Run Jaeger

There are many possible servers, for now we only support running a local Jaeger instance.

Start this in a terminal to run the server in the background:

```
# optionally specify 1.45 rather than latest
docker run -d -p6831:6831/udp -p6832:6832/udp -p16686:16686 -p14268:14268 jaegertracing/all-in-one:latest
```

You should now see the Web interface at http://localhost:16686


**Note** If you enable jaeger, all logs will go to that server via an async batch API. Nothing will be written out to the shell, as it added too much overhead for tracing information.

## Run Slay3rd with OpenTelemetry

You can enable jaeger tracing either with `--jaeger` flag or settings `SLAY_JAEGER=true` in the environment.
Since you want to see real-world numbers, let's compile release mode:

```bash
# this will make runtime a bit faster if we don't want low-level tracing info
cargo install --path . --features no-trace
cargo install --path .
SLAY_LOG=debug slay3rd --jaeger
```

Or with rocksdb:

```bash
SLAY_LOG=debug slay3rd --jaeger --rocksdb ~/.slay3r-test/rocks-1
```

## Test with integration tests

Check out the setup for [Integration Tests](../../integration/README.md) and initialize and run CometBFT
as they stated. When you run the test suite, you should generate activity in the Jaeger UI.
