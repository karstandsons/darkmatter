#!/bin/sh
# A dress rehearsal of the VPS on this machine: deploy/axiond's container,
# with Pebble standing in for Let's Encrypt.
#
#   deploy/axiond/rehearsal/rehearse.sh up       build and start; waits for the certificate
#   deploy/axiond/rehearsal/rehearse.sh axiond … run an axiond command inside (setup, invite…)
#   deploy/axiond/rehearsal/rehearse.sh ca       Pebble's root, for SSL_CERT_FILE
#   deploy/axiond/rehearsal/rehearse.sh down     stop, keeping the data (-v to drop it too)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
compose() { docker compose --project-directory "$here/.." -f "$here/../compose.yaml" -f "$here/compose.yaml" "$@"; }

case ${1:-} in
up)
    # Pebble's own HTTPS certificate, so axiond trusts the ACME server.
    id=$(docker create ghcr.io/letsencrypt/pebble:latest)
    docker cp "$id:/test/certs/pebble.minica.pem" "$here/minica.pem" >/dev/null
    docker rm "$id" >/dev/null
    compose up -d --build
    printf 'waiting for the certificate'
    for _ in $(seq 60); do
        if compose logs axiond 2>&1 | grep -q 'certificate: DeployedNewCert\|certificate: DeployedCachedCert'; then
            echo ' got it'; exit 0
        fi
        printf .; sleep 2
    done
    echo; compose logs axiond | tail -20; exit 1
    ;;
axiond) shift; compose exec axiond axiond "$@" ;;
ca)
    # Pebble makes a new root each time it starts; certificates chain to it.
    curl -sfk https://127.0.0.1:15000/roots/0
    ;;
down) shift; compose down "$@" ;;
*) compose "$@" ;;
esac
