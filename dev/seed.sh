#!/usr/bin/env bash
# Start a throwaway Mattermost and seed it: users alice (admin) and bob,
# team "dev" with channel ~rust. Dev-only credentials.
#   dev/seed.sh            seed once (idempotent)
#   dev/seed.sh --bulk N   also fill ~bulk with N messages (paging/perf tests)
set -euo pipefail
BULK=0
[[ ${1:-} == --bulk ]] && BULK=${2:?--bulk needs a count}

U=http://localhost:8065/api/v4
P='Devpass-123!'

docker start mm-dev 2>/dev/null || docker run -d --name mm-dev -p 8065:8065 mattermost/mattermost-preview
until curl -sf $U/system/ping >/dev/null; do sleep 2; done

login() {
  curl -s -i -X POST $U/users/login -H 'Content-Type: application/json' \
    -d "{\"login_id\":\"$1\",\"password\":\"$P\"}" | awk -F': ' 'tolower($1)=="token"{print $2}' | tr -d '\r'
}
j() { curl -s -H "Authorization: Bearer $T" -H 'Content-Type: application/json' "$@"; }

# The first user on a fresh server becomes system admin.
curl -s -X POST $U/users -H 'Content-Type: application/json' \
  -d "{\"email\":\"alice@example.com\",\"username\":\"alice\",\"password\":\"$P\"}" >/dev/null
T=$(login alice)
bulk() {
  local team ch
  team=$(j "$U/teams/name/dev" | jq -r .id)
  ch=$(j "$U/teams/$team/channels/name/bulk" | jq -r 'select(.team_id) | .id')
  [[ -n $ch ]] || ch=$(j -X POST $U/channels -d "{\"team_id\":\"$team\",\"name\":\"bulk\",\"display_name\":\"Bulk\",\"type\":\"O\"}" | jq -r .id)
  echo "posting $BULK messages to ~bulk…"
  seq 1 "$BULK" | xargs -P 8 -I{} curl -s -o /dev/null -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
    -X POST $U/posts -d "{\"channel_id\":\"$ch\",\"message\":\"bulk message #{} with **some** markdown and a :sparkles:\"}"
  echo "done: $(j "$U/channels/$ch" | jq -r .total_msg_count) messages in ~bulk"
}

if [[ $(j "$U/teams/name/dev/exists" | jq -r .exists) == true ]]; then
  echo "already seeded"
  [[ $BULK -gt 0 ]] && bulk
  exit 0
fi

BOB=$(j -X POST $U/users -d "{\"email\":\"bob@example.com\",\"username\":\"bob\",\"password\":\"$P\"}" | jq -r .id)
TEAM=$(j -X POST $U/teams -d '{"name":"dev","display_name":"Dev Team","type":"O"}' | jq -r .id)
j -X POST $U/teams/$TEAM/members -d "{\"team_id\":\"$TEAM\",\"user_id\":\"$BOB\"}" >/dev/null
CH=$(j -X POST $U/channels -d "{\"team_id\":\"$TEAM\",\"name\":\"rust\",\"display_name\":\"Rust\",\"type\":\"O\"}" | jq -r .id)
j -X POST $U/channels/$CH/members -d "{\"user_id\":\"$BOB\"}" >/dev/null
TS=$(j "$U/teams/$TEAM/channels/name/town-square" | jq -r .id)
j -X POST $U/posts -d "{\"channel_id\":\"$TS\",\"message\":\"Welcome to **mm-rust** dev :wave:\"}" >/dev/null
echo "seeded: login alice or bob, password $P"
[[ $BULK -gt 0 ]] && bulk
