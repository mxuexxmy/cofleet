#!/bin/sh
set -eu
# exec routing starts this inside the isolated app container. No host-network
# listener: only lzcinit in this container can reach this backend.
set --
if [ -n "${COFLEET_ACCESS_DOMAIN:-}" ]; then
  set -- --access-domain "$COFLEET_ACCESS_DOMAIN"
fi
exec /lzcapp/pkg/content/cofleet-manager \
  --listen 127.0.0.1:9760 \
  --auth lazycat \
  --state-dir /lzcapp/var/cofleet \
  --trusted-ingress-ip 127.0.0.1 \
  "$@"
