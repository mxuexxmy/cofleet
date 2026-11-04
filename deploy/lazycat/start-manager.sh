#!/bin/sh
set -eu
# exec routing starts this inside the isolated app container. No host-network
# listener: only lzcinit in this container can reach this backend.
set --
if [ -n "${COFLEET_ACCESS_DOMAIN:-}" ]; then
  set -- --access-domain "$COFLEET_ACCESS_DOMAIN"
else
  # The verified target cannot route prefix hosts when the assigned base label has '-'.
  # Keep legacy installations in observation mode; use a plain label for access deployments.
  case "${LAZYCAT_APP_DOMAIN:?missing assigned application domain}" in
    *.*) base_label=${LAZYCAT_APP_DOMAIN%%.*} ;;
    *) exit 1 ;;
  esac
  case "$base_label" in
    *-*) ;;
    *) set -- --trusted-app-prefix --management-domain "$LAZYCAT_APP_DOMAIN" ;;
  esac
fi
exec /lzcapp/pkg/content/cofleet-manager \
  --listen 127.0.0.1:9760 \
  --auth lazycat \
  --state-dir /lzcapp/var/cofleet \
  --trusted-ingress-ip 127.0.0.1 \
  "$@"
