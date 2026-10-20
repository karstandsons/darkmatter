#!/bin/sh
# Copies axiond's backups off this server, encrypted, with restic.
#
# axiond already writes a backup into data/backups every day and keeps a
# week of them. This sends them somewhere else, so losing the VPS doesn't
# lose the chat. restic encrypts before anything leaves; keep its password
# somewhere safe that isn't this server (both admins should have it).
#
# Needs: RESTIC_REPOSITORY and RESTIC_PASSWORD_FILE in /etc/axiond-backup.env,
# plus whatever your storage needs (e.g. AWS_ACCESS_KEY_ID for S3/B2/R2).
set -eu
cd "$(dirname "$0")"
. /etc/axiond-backup.env
export RESTIC_REPOSITORY RESTIC_PASSWORD_FILE

# A fresh backup now, so the copy is never more than a run old.
docker compose exec -T axiond axiond backup
restic snapshots >/dev/null 2>&1 || restic init
restic backup --tag axiond --host axiond data/backups
# Keep 14 daily, 8 weekly and 12 monthly copies.
restic forget --tag axiond --prune --keep-daily 14 --keep-weekly 8 --keep-monthly 12
# Read back a sample, so a broken repository is noticed.
restic check --read-data-subset=1/20
