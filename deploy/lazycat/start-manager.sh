#!/bin/sh
set -eu
# exec routing starts this inside the isolated app container. No host-network
# listener: only lzcinit in this container can reach this backend.
exec /lzcapp/pkg/content/cofleet-manager \
  --listen 127.0.0.1:9760 \
  --auth lazycat \
  --trusted-ingress-ip 127.0.0.1
