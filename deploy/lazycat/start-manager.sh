#!/bin/sh
set -eu
# exec routing starts this inside the isolated app container. No host-network
# listener: only lzcinit in this container can reach this backend.
set --
if [ -n "${COFLEET_ACCESS_DOMAIN:-}" ]; then
  set -- --access-domain "$COFLEET_ACCESS_DOMAIN"
else
  # A migrated legacy deployment uses "test" as platform base while retaining
  # "cofleet-test" as its canonical management and Agent address.
  assigned_domain=${LAZYCAT_APP_DOMAIN:?missing assigned application domain}
  base_label=${assigned_domain%%.*}
  case "$base_label" in
    test) set -- --trusted-app-prefix --management-domain "cofleet-$assigned_domain" --access-prefix-domain "$assigned_domain" ;;
    *-*) ;; # Unmigrated legacy deployments retain observation until the domain migration.
    *) set -- --trusted-app-prefix --management-domain "$assigned_domain" ;;
  esac
fi
exec /lzcapp/pkg/content/cofleet-manager \
  --listen 127.0.0.1:9760 \
  --auth lazycat \
  --state-dir /lzcapp/var/cofleet \
  --trusted-ingress-ip 127.0.0.1 \
  "$@"
