#!/usr/bin/env bash
# adoption/run.sh --fixtures DIR [--image IMAGE] [--only NAME] [--user USERNAME]
#
# Adopts every Jellyfin generation Ferrofin claims to support, through one image, on a FRESH
# copy of each pristine fixture under DIR, and checks each one the same way:
#   1. the boot log names the expected generation, applies every migration, logs no ERROR;
#   2. smoke.sh answers match Jellyfin 12.1's own answers on the same library
#      (DIR/oracle/smoke-jellyfin-12.1.txt), ignoring lines that legitimately differ
#      (server version, task/plugin lists, /Devices, folder order, activity-log count,
#      image byte size);
#   3. PRAGMA integrity_check / foreign_key_check are clean on the adopted file;
#   4. a second boot runs no repair and changes no answer.
# One PASS/FAIL line per fixture; exit status is non-zero if any failed. Roughly two minutes
# per fixture. The fixtures are NOT in the repository — see adoption/README.md for what DIR
# must contain and how build-fixtures.sh derives everything from one 10.11.8 snapshot.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
FIXTURES=${FERROFIN_ADOPTION_FIXTURES:-}; IMAGE=${IMAGE:-ferrofin:bench}; ONLY=; USER_NAME=${ADOPTION_USER:-}
while [ $# -gt 0 ]; do case $1 in
  --fixtures) FIXTURES=$2; shift 2;; --image) IMAGE=$2; shift 2;; --only) ONLY=$2; shift 2;; --user) USER_NAME=$2; shift 2;;
  -h|--help) sed -n 2,16p "$0"; exit 0;; *) echo "run: unknown argument $1" >&2; exit 2;; esac; done
[ -n "$FIXTURES" ] || { echo "run: --fixtures DIR (or FERROFIN_ADOPTION_FIXTURES) is required" >&2; exit 2; }
FIXTURES=$(cd "$FIXTURES" && pwd)
for t in docker sqlite3 jq curl; do command -v $t >/dev/null || { echo "run: $t not installed" >&2; exit 2; }; done
docker image inspect "$IMAGE" >/dev/null 2>&1 || { echo "run: image $IMAGE missing (docker build -t ferrofin:bench .)" >&2; exit 2; }
ORACLE=$FIXTURES/oracle/smoke-jellyfin-12.1.txt
[ -f "$ORACLE" ] || { echo "run: $ORACLE missing — run adoption/build-fixtures.sh first" >&2; exit 2; }
# the probes must run as the account the oracle ran as; the builder records it
[ -n "$USER_NAME" ] || [ ! -f "$FIXTURES/oracle/user.txt" ] || USER_NAME=$(cat "$FIXTURES/oracle/user.txt")
MEDIA=(); [ -f "$FIXTURES/media-mounts.sh" ] && . "$FIXTURES/media-mounts.sh"
# name|fixture directory|host port
FIXTURE_TABLE=(
  "10.11.8|jellyfin-10.11.8|18099"
  "10.11.11|jellyfin-10.11.11-synthetic|18098"
  "12.0.0|jellyfin-12.0|18097"
  "12.1.0|jellyfin-12.1-from-10|18093"
  "12.1.0|jellyfin-12.1-from-12|18092"
)
# answers that legitimately differ between Ferrofin and Jellyfin, or between two boots
IGNORE='/System/Info|/ScheduledTasks|/Plugins|/Devices|/Library/VirtualFolders|/System/ActivityLog|/Sessions'
normalise() { sed -E 's/[0-9a-f]{32}/<id>/g; s/\(bytes=[0-9]+\)/(bytes)/' "$1" | grep -Ev "$IGNORE"; }
wait_ready() { local port=$1 i; for i in $(seq 1 300); do curl -sf "http://127.0.0.1:$port/System/Info/Public" >/dev/null && return 0; sleep 2; done; return 1; }
settle() { local port=$1 auth=$2 i busy; for i in $(seq 1 60); do busy=$(curl -sf "http://127.0.0.1:$port/ScheduledTasks" -H "$auth" 2>/dev/null | jq -r '[.[]|select(.State!="Idle")]|length' 2>/dev/null); [ "${busy:-1}" = 0 ] && break; sleep 2; done; sleep 5; }
REPAIR_RE='imported playlist|repaired |rewrote |recomputed |merged |backfilled |dropped dead|consolidat|reverted'
failed=0
for spec in "${FIXTURE_TABLE[@]}"; do
  IFS='|' read -r expected src port <<<"$spec"
  [ -z "$ONLY" ] || [ "$ONLY" = "$src" ] || [ "$ONLY" = "$expected" ] || continue
  [ -d "$FIXTURES/$src" ] || { printf '%-5s %-9s %-28s %s\n' SKIP "$expected" "$src" "fixture missing"; continue; }
  name=adopt-$src; dst=$FIXTURES/work/$src
  docker rm -f "$name" >/dev/null 2>&1; rm -rf "$dst" "$dst-cache"; mkdir -p "$FIXTURES/work"
  cp -a "$FIXTURES/$src" "$dst"; mkdir -p "$dst-cache"
  docker run -d --name "$name" --user "$(id -u):$(id -g)" -p "127.0.0.1:$port:8096" \
    -e FERROFIN_DATA_DIR=/config -e FERROFIN_CACHE_DIR=/cache \
    -v "$dst:/config" -v "$dst-cache:/cache" "${MEDIA[@]}" "$IMAGE" >/dev/null
  verdict=PASS; why=()
  wait_ready "$port" || { verdict=FAIL; why+=("never became ready"); }
  APIKEY=$(sqlite3 -readonly "file:$dst/data/jellyfin.db?mode=ro" 'SELECT AccessToken FROM ApiKeys ORDER BY DateCreated DESC LIMIT 1')
  AUTH="Authorization: MediaBrowser Token=\"$APIKEY\", Client=\"adoption\", Device=\"adoption\", DeviceId=\"adoption\", Version=\"1\""
  settle "$port" "$AUTH"
  log=$(docker logs "$name" 2>&1)
  gen=$(grep -o '"generation":"[^"]*"' <<<"$log" | head -1 | cut -d'"' -f4)
  [ "$gen" = "$expected" ] || { verdict=FAIL; why+=("generation '$gen' != '$expected'"); }
  grep -q '"database migrations applied"' <<<"$log" || { verdict=FAIL; why+=("migrations did not complete"); }
  grep -Eq '"level":"ERROR"' <<<"$log" && { verdict=FAIL; why+=("ERROR in boot log"); }
  "$HERE/smoke.sh" "http://127.0.0.1:$port" "$dst" "$USER_NAME" > "$dst.smoke.txt" 2>&1
  diff -q <(normalise "$ORACLE") <(normalise "$dst.smoke.txt") >/dev/null || { verdict=FAIL; why+=("smoke differs from Jellyfin 12.1 (diff $ORACLE $dst.smoke.txt)"); }
  # IX_Peoples_NameLower is an expression index on lower("Name"): the host CLI's lower() may be
  # ICU-aware while the servers' is not, so its rows are reported "missing from index" on a
  # file both servers agree with. Only that index is exempt.
  ic=$(sqlite3 -readonly "file:$dst/data/jellyfin.db?mode=ro" 'PRAGMA integrity_check' | grep -v 'missing from index IX_Peoples_NameLower' | head -1)
  [ -z "$ic" ] || [ "$ic" = ok ] || { verdict=FAIL; why+=("integrity_check: $ic"); }
  fk=$(sqlite3 -readonly "file:$dst/data/jellyfin.db?mode=ro" 'PRAGMA foreign_key_check' | wc -l)
  [ "$fk" = 0 ] || { verdict=FAIL; why+=("foreign_key_check: $fk rows"); }
  docker restart "$name" >/dev/null; wait_ready "$port" || { verdict=FAIL; why+=("second boot never ready"); }
  settle "$port" "$AUTH"
  second=$(docker logs --since "$(date -u -d '-90 seconds' +%Y-%m-%dT%H:%M:%S)" "$name" 2>&1 | grep -E "$REPAIR_RE" | grep -v '"repaired":0' | head -1)
  [ -z "$second" ] || { verdict=FAIL; why+=("second boot repaired again: $(cut -c1-120 <<<"$second")"); }
  "$HERE/smoke.sh" "http://127.0.0.1:$port" "$dst" "$USER_NAME" > "$dst.smoke2.txt" 2>&1
  diff -q <(normalise "$dst.smoke.txt") <(normalise "$dst.smoke2.txt") >/dev/null || { verdict=FAIL; why+=("second boot answers differ"); }
  docker logs "$name" > "$dst.server.log" 2>&1; docker rm -f "$name" >/dev/null
  printf '%-5s %-9s %-28s %s\n' "$verdict" "$expected" "$src" "${why[*]:-}"
  if [ "$verdict" = PASS ]; then rm -rf "$dst" "$dst-cache"; else failed=1; fi
done
exit $failed
