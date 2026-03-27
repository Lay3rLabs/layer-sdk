#!/bin/bash
set -eux

# You need a personal access token (classic) from github.
# https://github.com/settings/tokens
# docker login -u USERNAME --password-stdin ghcr.io

SUDO="sudo"
if groups | grep -q docker; then
  SUDO=""
fi

TAG=${TAG:-}

for img in gateway layerd; do
  $SUDO docker push ghcr.io/lay3rlabs/${img}:latest

  if [ -n "$TAG" ]; then
    $SUDO docker tag ghcr.io/lay3rlabs/${img}:latest ghcr.io/lay3rlabs/${img}:$TAG
    $SUDO docker push ghcr.io/lay3rlabs/${img}:$TAG
  fi
done
