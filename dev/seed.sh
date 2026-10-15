#!/usr/bin/env bash
# Start a throwaway Mattermost and seed it: users alice (admin) and bob,
# team "dev" with channel ~rust. Dev-only credentials.
#   dev/seed.sh            seed once (idempotent)
#   dev/seed.sh --bulk N   also fill ~bulk with N messages (paging/perf tests)
#   dev/seed.sh --threads  also fill ~threads with roots that have 1-40 replies
set -euo pipefail
BULK=0 THREADS=0
case ${1:-} in
  --bulk) BULK=${2:?--bulk needs a count} ;;
  --threads) THREADS=1 ;;
esac

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

# A busy channel of threads from alice, bob and carol, for inline-thread tests.
threads() {
  local team ch who root n i
  team=$(j "$U/teams/name/dev" | jq -r .id)
  ch=$(j "$U/teams/$team/channels/name/threads" | jq -r 'select(.team_id) | .id')
  if [[ -n $ch ]]; then echo "~threads already exists"; return; fi
  ch=$(j -X POST $U/channels -d "{\"team_id\":\"$team\",\"name\":\"threads\",\"display_name\":\"Threads\",\"type\":\"O\"}" | jq -r .id)
  declare -A tok
  for who in alice bob carol; do
    curl -s -o /dev/null -X POST $U/users -H 'Content-Type: application/json' \
      -d "{\"email\":\"$who@example.com\",\"username\":\"$who\",\"password\":\"$P\"}"
    tok[$who]=$(login $who)
    local id; id=$(j "$U/users/username/$who" | jq -r .id)
    j -X POST $U/teams/$team/members -d "{\"team_id\":\"$team\",\"user_id\":\"$id\"}" >/dev/null
    j -X POST $U/channels/$ch/members -d "{\"user_id\":\"$id\"}" >/dev/null
  done
  local people=(alice bob carol) sizes=(1 3 0 12 2 0 40 5 1 0 8 3 25 0 1 6)
  post() { curl -s -H "Authorization: Bearer ${tok[$1]}" -H 'Content-Type: application/json' -X POST $U/posts -d "$2"; }
  for i in $(seq 0 39); do
    who=${people[$((i % 3))]} n=${sizes[$((i % ${#sizes[@]}))]}
    root=$(post $who "{\"channel_id\":\"$ch\",\"message\":\"Topic $i from $who: thoughts on item $i?\"}" | jq -r .id)
    for r in $(seq 1 $n); do
      post ${people[$(((i + r) % 3))]} "{\"channel_id\":\"$ch\",\"root_id\":\"$root\",\"message\":\"reply $r to topic $i\"}" >/dev/null
    done
  done
  echo "seeded ~threads: 40 topics with up to 40 replies"
}

if [[ $(j "$U/teams/name/dev/exists" | jq -r .exists) == true ]]; then
  echo "already seeded"
  [[ $BULK -gt 0 ]] && bulk
  [[ $THREADS -gt 0 ]] && threads
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
[[ $THREADS -gt 0 ]] && threads
