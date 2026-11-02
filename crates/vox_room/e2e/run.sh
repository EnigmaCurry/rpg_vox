#!/usr/bin/env bash
# Run one of the room's end-to-end suites, or all of them:
#
#   e2e/run.sh              # every suite that needs nothing of yours
#   e2e/run.sh rooms door   # just these
#
# Each suite wants a room started a particular way and a pair of fresh
# invite links -- a link is spent by Join Audio, so a second run on the
# same pair gets the expired page. This starts a room of its own on a
# scratch database, mints what the suite needs, runs it, and stops the
# room again. Nothing here touches the database you actually use.
#
# The suites that need a microphone, a model or a voice are not in the
# default set: see `two-people`, `hear-out`, `interpret`, `share-screen`
# below and the comments at the top of each.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
crate=$(dirname "$here")
root=$(dirname "$(dirname "$crate")")
bin=${VOX_ROOM_BIN:-$root/target/release/vox_room}
port=${VOX_ROOM_E2E_PORT:-7349}
base=http://127.0.0.1:$port

[[ -x $bin ]] || {
  echo "no vox_room at $bin -- cargo build -p vox_room --release" >&2
  exit 1
}
# Playwright is nobody's dependency here -- nothing built in this repo
# needs it -- so it is installed beside the scripts and left out of git.
(cd "$here" && node -e 'require.resolve("playwright")') >/dev/null 2>&1 || {
  echo "needs playwright: (cd $here && npm i playwright)" >&2
  echo "and a chromium it can launch: CHROME=\$(which chromium) $0 $*" >&2
  exit 1
}

# What each suite wants the room to be.
flags() {
  case $1 in
    interpreter)
      echo "--room Lobby --room Studio --no-stt --tts qwen3 --tts-url http://127.0.0.1:1
            --model fake --llm-url http://127.0.0.1:1/v1
            --language English --language-also Russian --translate openai" ;;
    door)
      echo "--room Lobby --no-stt --tts qwen3 --tts-url http://127.0.0.1:1
            --model fake --llm-url http://127.0.0.1:1/v1
            --language English --language-also Russian --translate openai" ;;
    rooms)   echo "--room Main --room Studio --echo --no-stt --tts none" ;;
    # A short back-and-forth: being named has one AI answering the next
    # pause as well, and this script is about which of them answers.
    bots)    echo "--room Lobby --bot Sage --echo --no-stt --tts none
                   --engaged-for-ms 600" ;;
    *)       echo "--room Lobby --room Studio --echo --no-stt --tts none" ;;
  esac
}

# Who each suite wants invited, in the order it takes them.
guests() {
  case $1 in
    door) echo "Fresh Known En Ru Fr" ;;
    permalinks|ptt|settings) echo "Ryan" ;;
    *) echo "Ryan Alex" ;;
  esac
}

server=
stop() { [[ -n $server ]] && kill "$server" 2>/dev/null; server=; }
trap stop EXIT

one() {
  local suite=$1 data links=() who
  data=$(mktemp -d)
  export VOX_ROOM_DATA=$data VOX_ROOM_LISTEN=127.0.0.1:$port VOX_ROOM_URL=$base

  for who in $(guests "$suite"); do links+=("$("$bin" invite "$who")"); done

  # Alex reads in the room's second language, so there is somebody for
  # the interpreter to interpret to.
  if [[ $suite == interpreter ]]; then
    sqlite_set "$data/room.db" "UPDATE people SET language='Russian' WHERE name='Alex';"
  fi

  # A game to play has to exist before anybody can play it, and it is
  # announced by a card in the conversation it was designed in.
  if [[ $suite == game ]]; then
    "$bin" $(flags "$suite") >/dev/null 2>&1 &
    server=$!
    waitfor; stop
    local c
    c=$(sqlite_get "$data/room.db" "SELECT value FROM settings WHERE key='Lobby:conversation';")
    sqlite_set "$data/room.db" \
      "INSERT INTO games (name,summary,prompt,created,played,settings)
         VALUES ('Quick Draw','A showdown at noon.','Run a showdown.',1,0,'{}');
       INSERT INTO entries (conversation,id,who,kind,text,at)
         VALUES ($c,'g1','Vox','game','Quick Draw',1);"
  fi

  echo "=== $suite"
  # shellcheck disable=SC2046
  "$bin" $(flags "$suite") >"$data/room.log" 2>&1 &
  server=$!
  waitfor || { echo "the room didn't start; see $data/room.log" >&2; return 1; }
  # From beside the scripts, so `require` finds both the helper and
  # whatever playwright was installed there.
  (cd "$here" && VOX_ROOM_URL=$base node "$suite.js" "${links[@]}")
  local code=$?
  stop
  rm -rf "$data"
  return $code
}

waitfor() {
  local i
  for i in $(seq 60); do
    curl -sf "$base/healthz" >/dev/null && return 0
    sleep 0.25
  done
  return 1
}

# sqlite, from nix when the host hasn't got one.
sqlite() {
  if command -v sqlite3 >/dev/null; then sqlite3 "$@"; else nix run nixpkgs#sqlite -- "$@"; fi
}
sqlite_get() { sqlite "$1" "$2"; }
sqlite_set() { sqlite "$1" "$2" >/dev/null; }

suites=("$@")
[[ ${#suites[@]} -eq 0 ]] && suites=(rooms bots door permalinks ptt settings conversations game interpreter)

bad=0
for suite in "${suites[@]}"; do
  [[ -f $here/$suite.js ]] || { echo "no suite called $suite" >&2; bad=1; continue; }
  one "$suite" || bad=1
done
[[ $bad -eq 0 ]] && echo && echo "every suite passed"
exit $bad
