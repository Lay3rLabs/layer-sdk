#!/bin/bash
set -eu

# This generates grpc-gateway bindings in the gateway directory 

IMAGE="bufbuild/buf:1.23.1"

SUDO="sudo"
if groups | grep -q docker; then
  SUDO=""
fi

PROTOSPEC=$(pwd)/packages/cosmossdk/tools/protospec
GATEWAY=$(pwd)/packages/cosmossdk/tools/gateway

$SUDO docker run --rm -w /buf/proto -v "$PROTOSPEC:/buf/proto" -v "$GATEWAY:/buf/gateway" "$IMAGE" ls-files
$SUDO docker run --rm -w /buf/proto -v "$PROTOSPEC:/buf/proto" -v "$GATEWAY:/buf/gateway" "$IMAGE" generate

# change it back if we ran as root before
if [ -n "$SUDO" ]; then
  WHOAMI=$(whoami)
  sudo chown -R $WHOAMI:$WHOAMI "$GATEWAY"
fi
