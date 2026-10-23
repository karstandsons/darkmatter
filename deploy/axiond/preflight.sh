#!/bin/sh
# Checks a server before axiond's first start, on any provider and any Linux:
# Docker, the settings in .env, the data folder, port 443, memory for the
# build, and whether the domain points here. Changes nothing.
#
#   cd deploy/axiond && ./preflight.sh
set -u
cd "$(dirname "$0")"
problems=0
ok() { printf '  ok    %s\n' "$*"; }
warn() { printf '  WARN  %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; problems=$((problems + 1)); }

echo "Docker"
if ! command -v docker >/dev/null 2>&1; then
    bad "docker isn't installed (https://docs.docker.com/engine/install/)"
elif ! docker info >/dev/null 2>&1; then
    bad "can't talk to Docker: run as root, or add yourself to the docker group and log in again"
elif ! docker compose version >/dev/null 2>&1; then
    bad "the compose plugin is missing (docker-compose-plugin)"
else
    ok "$(docker compose version)"
fi

echo "Settings (.env)"
domain=
if [ ! -f .env ]; then
    bad "no .env: cp .env.example .env, then edit it"
else
    domain=$(sed -n 's/^AXIOND_DOMAIN=//p' .env | tr -d '"'"'"' ')
    email=$(sed -n 's/^AXIOND_ACME_EMAIL=//p' .env | tr -d '"'"'"' ')
    case $domain in
    '') bad "AXIOND_DOMAIN is empty" ;;
    *example.com) bad "AXIOND_DOMAIN is still the example ($domain)" ;;
    *) ok "domain $domain" ;;
    esac
    case $email in
    '' | *example.com) warn "AXIOND_ACME_EMAIL is empty or the example: you won't hear if renewing the certificate fails" ;;
    *) ok "certificate notices go to $email" ;;
    esac
    if grep -q '^AXIOND_ACME_STAGING=true' .env; then
        ok "staging certificate first (switch to false once it works)"
    fi
    public=$(sed -n 's/^AXIOND_CALLS_PUBLIC_IP=//p' .env | tr -d '"'"'"' ')
    if [ -z "$public" ]; then
        warn "AXIOND_CALLS_PUBLIC_IP is empty: calls are off (this server's addresses: $(hostname -I 2>/dev/null))"
    else
        ok "calls' media goes to $public, 443/udp"
    fi
fi

echo "Data folder"
if [ ! -d data ]; then
    bad "no data/: sudo install -d -o 65532 -g 65532 -m 0700 data"
else
    owner=$(stat -c '%u %a' data 2>/dev/null)
    if [ "$owner" = "65532 700" ]; then ok "data/ belongs to the server's user, private"; else bad "data/ is '$owner' (owner, mode), not '65532 700': sudo chown 65532:65532 data && sudo chmod 700 data"; fi
fi

echo "Port 443"
if command -v ss >/dev/null 2>&1; then
    taken=$(ss -Hltn 'sport = :443' 2>/dev/null)
    if [ -z "$taken" ]; then ok "free"
    elif docker compose ps --status running 2>/dev/null | grep -q axiond; then ok "axiond already has it"
    else bad "something else listens on 443 (a web server?): $(echo "$taken" | head -1)"; fi
else
    warn "can't check (no ss)"
fi

echo "Memory"
# Building axiond on the server needs about this much (memory + swap):
# it built in 8 minutes with 1 GB and 1 CPU.
need_mb=900
have_mb=$(awk '/^MemTotal:|^SwapTotal:/ { kb += $2 } END { print int(kb / 1024) }' /proc/meminfo)
if docker image inspect axiond:local >/dev/null 2>&1; then
    ok "image already built"
elif [ "$have_mb" -ge "$need_mb" ]; then
    ok "${have_mb} MB with swap, enough to build"
else
    bad "${have_mb} MB with swap; building needs about ${need_mb}. Add swap: sudo fallocate -l 1G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile (and a line in /etc/fstab)"
fi

echo "Name"
if [ -n "$domain" ] && ! echo "$domain" | grep -q example.com; then
    addrs=$(getent ahosts "$domain" 2>/dev/null | awk '{ print $1 }' | sort -u)
    if [ -z "$addrs" ]; then
        bad "$domain doesn't resolve yet: add an A record (and AAAA if the server has IPv6) at your DNS provider"
    else
        ok "$domain -> $(echo $addrs)"
        mine=$(ip -o addr show 2>/dev/null | awk '{ sub(/\/.*/, "", $4); print $4 }')
        match=
        for a in $addrs; do echo "$mine" | grep -qx "$a" && match=$a; done
        if [ -n "$match" ]; then
            ok "$match is this server"
        else
            warn "none of those addresses is on this server's network cards. Fine if your provider puts a public address in front (AWS, Google, Oracle, Azure do): check it's the one in your provider's panel."
        fi
    fi
fi

echo "Reaching Let's Encrypt"
if command -v curl >/dev/null 2>&1; then
    if curl -fsS -m 10 -o /dev/null https://acme-v02.api.letsencrypt.org/directory; then ok "reachable"; else bad "can't reach acme-v02.api.letsencrypt.org (outgoing HTTPS blocked?)"; fi
else
    warn "can't check (no curl)"
fi

echo
echo "Outside this server, check by hand: if your provider has a firewall in its"
echo "control panel (most clouds do), it must allow 22/tcp, 443/tcp and 443/udp in."
echo
if [ "$problems" -eq 0 ]; then
    echo "Ready: docker compose up -d --build"
else
    echo "$problems thing(s) to fix first."
    exit 1
fi
