# Running axiond on a VPS

A checklist for putting axiond on a small virtual server, run by a couple of
admins. It takes about an hour. To try it all on your own machine first, with
Pebble (Let's Encrypt's test server) in place of Let's Encrypt, run
`rehearsal/rehearse.sh up`; the script explains the rest.

## 1. The server

Any provider and any domain will do. axiond runs in a container, so all it
needs is a Linux machine with Docker and a name pointing at it.

- **Size:** the smallest plans work: 1 vCPU and 1 GB of memory runs a
  group's chat, and builds axiond in about 8 minutes (5 with 2 vCPU and
  2 GB). With less memory, add swap first (`preflight.sh` says how). x86_64
  and arm64 both work.
- **Traffic, for calls:** monthly traffic matters more than CPU. Video is
  for up to 5 people at a time; a 5-person video call sends up to about
  7 GB an hour (cameras are capped at 800 kbps), and voice alone well under
  1 GB, so even a plan with 1 TB a month covers well over 100 hours of video
  calls. One SFU thread carries several such calls on a small VPS.
- **Address:** a public IPv4 address. IPv6 as well is good. IPv6 alone
  isn't enough, because people on IPv4-only networks couldn't reach it.
- **System:** any current Linux that Docker supports. The commands below are
  for Debian/Ubuntu and for Fedora/Rocky/Alma.
- **A name:** any domain or subdomain you control, at any registrar or DNS
  host, such as `chat.example.com`. A free dynamic-DNS name works too, as long
  as you can point it at the server. Add an A record (and an AAAA record if
  the server has IPv6) with the server's public address, before the first
  start.
- **The provider's firewall:** many providers have one in their control
  panel, outside the server (AWS, Google Cloud, Azure and Oracle always do;
  DigitalOcean and Hetzner optionally). Allow 22/tcp, 443/tcp and 443/udp in there as
  well, or Let's Encrypt can't reach the server.

## 2. Lock it down

Install Docker first, from Docker's own repository
(https://docs.docker.com/engine/install/), so its group exists.

Each admin gets their own login with their own SSH key. Nobody shares one.

```sh
# As root, once per admin:
#   Debian/Ubuntu:
adduser --disabled-password --gecos '' alex && usermod -aG sudo,docker alex
#   Fedora/Rocky/Alma:
useradd -m -G wheel,docker alex
# Then, on either:
install -d -m 700 -o alex -g alex /home/alex/.ssh
# put alex's public key in /home/alex/.ssh/authorized_keys (mode 600, owned by alex)
```

Check you can log in as each admin with their key **before** the next step,
which turns off passwords and root logins:

```sh
printf 'PasswordAuthentication no\nPermitRootLogin no\n' > /etc/ssh/sshd_config.d/10-axiond.conf
systemctl reload ssh 2>/dev/null || systemctl reload sshd
```

A firewall that lets in only SSH, HTTPS and calls' media (443/udp), and
security updates installed automatically:

```sh
#   Debian/Ubuntu:
apt install -y ufw unattended-upgrades
ufw allow OpenSSH && ufw allow 443/tcp && ufw allow 443/udp && ufw enable
dpkg-reconfigure -plow unattended-upgrades
#   Fedora/Rocky/Alma:
dnf install -y firewalld dnf-automatic
systemctl enable --now firewalld
firewall-cmd --permanent --add-service=ssh --add-service=https --add-port=443/udp && firewall-cmd --reload
printf '[commands]\napply_updates = yes\n' > /etc/dnf/automatic.conf
systemctl enable --now dnf-automatic.timer
```

Docker opens the ports it publishes past these firewalls. That's why
`compose.yaml` publishes 443 only, TCP and UDP (and the Caddy variant
binds to 127.0.0.1). Calls use 443/udp, and on networks that block UDP
they go over 443/tcp alongside HTTPS: axiond tells the two apart.

## 3. Start axiond

```sh
sudo git clone <this repository> /opt/axiond && cd /opt/axiond/deploy/axiond
cp .env.example .env && $EDITOR .env          # your domain and email
sudo install -d -o 65532 -g 65532 -m 0700 data
./preflight.sh                                # checks all of the above
docker compose up -d --build
docker compose logs -f                        # watch for the certificate
```

`.env` starts with `AXIOND_ACME_STAGING=true`, which gets a test
certificate (apps will reject it) without using up Let's Encrypt's limits.
Once the logs show a certificate, set it to `false` and run
`docker compose up -d` again for a real one.

The container runs as an unprivileged user with a read-only filesystem and
no capabilities. Only `data/` is written: the database, uploads, the
certificate and backups.

## 4. People

```sh
# The first admin and the team:
docker compose exec axiond axiond setup --admin alex --email alex@example.com --team home --team-name Home
# The second admin:
docker compose exec axiond axiond user add sam --email sam@example.com --admin
# An invite code for each friend (one use, a week):
docker compose exec axiond axiond invite --note "for Jo"
```

`setup` also measures the machine and sets how hard passwords are to
check (argon2id, about a quarter of a second each). Wrong passwords lock an
account for a while after five, doubling up to an hour, and a restart
doesn't reset that; sessions end 90 days after signing in, however often
they're used. People change their own passwords in Darkmatter's Account
window.

Both admins should turn on two-factor codes for their accounts. Reports from
people come in through `axiond reports`, and `axiond audit` shows what each
admin did.

## 5. Backups that leave the server

axiond backs itself up every day into `data/backups` and keeps a week. That
doesn't survive losing the VPS, so copy them off, encrypted, with restic:

```sh
sudo apt install -y restic        # or: sudo dnf install -y restic (Rocky/Alma: from EPEL)
sudo install -m 600 /dev/null /etc/axiond-backup.env
sudo $EDITOR /etc/axiond-backup.env
#   RESTIC_REPOSITORY=s3:https://<bucket endpoint>/axiond   (any S3-compatible storage, or sftp:user@host:/path, or rest:…)
#   RESTIC_PASSWORD_FILE=/etc/axiond-restic-password
#   AWS_ACCESS_KEY_ID=…  AWS_SECRET_ACCESS_KEY=…
sudo sh -c 'head -c 32 /dev/urandom | base64 > /etc/axiond-restic-password && chmod 600 /etc/axiond-restic-password'
sudo cp axiond-backup.service axiond-backup.timer /etc/systemd/system/
sudo systemctl enable --now axiond-backup.timer
sudo systemctl start axiond-backup.service && journalctl -u axiond-backup -n 20
```

Keep the restic password somewhere that isn't the server; both admins
should have it. Without it the backups can't be read, by anyone.

Keep **`data/secret.key`** the same way (in both admins' password
managers, say). axiond makes it on first start and encrypts two-factor
secrets with it; backups leave it out, so a stolen backup doesn't hold
them. A restore without it still works, but axiond won't start until you
put it back or turn off the codes of the people it names
(`axiond user mfa-off <username>`).

**Practise a restore** every few months on a spare machine, with Docker,
restic, a checkout of this repository (`docker compose build` in
`deploy/axiond` makes `axiond:local`) and copies of `/etc/axiond-backup.env`
and the password file:

```sh
sudo sh -c 'set -a; . /etc/axiond-backup.env; restic restore latest --target /tmp/r'
# Backups are private files; hand them to the server's user (65532).
sudo chown -R 65532:65532 /tmp/r
sudo install -d -o 65532 -g 65532 -m 0700 /tmp/newdata
docker run --rm -v /tmp/r:/r:ro -v /tmp/newdata:/data axiond:local \
  restore /r/opt/axiond/deploy/axiond/data/backups/axiond-<newest>.tar.gz
```

Then copy your `secret.key` over the new one the restore made in
`/tmp/newdata` (owned by 65532, mode 600), point a test copy of the kit at
it and log in.

## 6. Updating

```sh
cd /opt/axiond && git pull && cd deploy/axiond && docker compose up -d --build
```

Before an update changes the database, axiond keeps a copy of it in
`data/backups/before-upgrade-<time>.db`.

## 7. Watching it

- `docker compose ps` shows `healthy` when it answers.
- Add an outside uptime check (any free monitor) on
  `https://chat.example.com/api/v4/system/ping`, which answers `{"status":"OK"}`.

## Caddy in front instead

If the server already runs Caddy (or Traefik, or nginx), let it do HTTPS:
remove `AXIOND_DOMAIN` from `.env`, set `AXIOND_BEHIND_PROXY=true`, publish
`127.0.0.1:8065:8065` instead of 443 with `AXIOND_LISTEN=0.0.0.0:8065`, and
in the Caddyfile:

```
chat.example.com {
    reverse_proxy 127.0.0.1:8065
}
```

Calls still need their UDP port published to axiond itself (a proxy for
HTTPS doesn't carry them): add `"443:443/udp"` under `ports`, with
`AXIOND_CALLS_UDP=0.0.0.0:443` and `AXIOND_CALLS_PUBLIC_IP` as above. Set
`AXIOND_CALLS_PUBLIC_TCP_PORT=0` too: calls over TCP need axiond on 443
itself, so behind a proxy calls can't get through networks that block
UDP.
