#!/bin/bash
set -eux

TAG=0.5.1

# You need a personal access token (classic) from github.
# https://github.com/settings/tokens
# docker login -u USERNAME --password-stdin ghcr.io

SUDO="sudo"
if groups | grep -q docker; then
  SUDO=""
fi

for img in gateway layerd faucet; do
  $SUDO docker tag ghcr.io/lay3rlabs/$img:latest ghcr.io/lay3rlabs/$img:$TAG
  $SUDO docker push ghcr.io/lay3rlabs/$img:$TAG
done
