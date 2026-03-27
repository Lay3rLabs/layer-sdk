# Proto Compiler

You need to run this when you update the `proto` dir in order to generate updated files in
`packages/proto`.

## Prerequisites

You need to install protoc somewhere in `$PATH`. Easiest is to 
[download a precompiled version here](https://github.com/protocolbuffers/protobuf/releases).

On Debian/Ubuntu systems, try: `sudo apt-get install protobuf-compiler`

## Usage

```bash
cd ./tools/proto-compiler
cargo run
```

