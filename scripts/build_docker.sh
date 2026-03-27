#!/bin/bash
set -eux

SUDO="sudo"
if groups | grep -q docker; then
  SUDO=""
fi

for img in gateway layerd; do
  $SUDO docker build . -f docker/Dockerfile.${img} -t ghcr.io/lay3rlabs/${img}:latest
done
