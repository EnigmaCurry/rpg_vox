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
    # Three rooms: the lobby everybody has, and two that are given out.
    roles)   echo "--room Lobby --room Studio --room Cellar --echo --no-stt --tts none" ;;
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
    roles) echo "[Ryan] Alex" ;;
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

  # A name in [square brackets] is the owner, invited with the command
  # that makes one: there is one owner and only the command line can
  # say who it is, which is the thing the roles suite is about.
  for who in $(guests "$suite"); do
    if [[ $who == \[*\] ]]; then
      links+=("$("$bin" invite-owner "${who:1:${#who}-2}" 2>/dev/null)")
    else
      links+=("$("$bin" invite "$who")")
    fi
  done

  # Rooms are given out by the owner now. Every suite but `roles` is
  # about something else and wants its people able to walk about the
  # house, so they are given every room here -- which is what an owner
  # would have done before any of these scripts ran.
  if [[ $suite != roles ]]; then
    local first=1 grant=""
    for room in $(flags "$suite" | tr ' ' '\n' | grep -A1 -x -- --room | grep -vx -- --room | grep -v '^--$'); do
      if [[ $first == 1 ]]; then first=0; continue; fi   # the lobby is everybody's
      for who in $(guests "$suite"); do
        grant+="INSERT OR IGNORE INTO grants (name, room) VALUES ('$who', '$room');"
      done
    done
    [[ -n $grant ]] && sqlite_set "$data/room.db" "$grant"
  fi

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

  # Voices are cloned from somebody reading into their microphone, and
  # there is nobody here doing that -- so the house is given two. What
  # the bots suite is about is the *choosing*: which voice one of them
  # speaks in here, which the house has settled on, and which of those
  # two wins. Nothing has to have been recorded for that.
  if [[ $suite == bots ]]; then
    "$bin" $(flags "$suite") >/dev/null 2>&1 &
    server=$!
    waitfor; stop
    sqlite_set "$data/room.db" \
      "INSERT INTO voices (id,name,who,said,created)
         VALUES ('alan-watts','Alan Watts','Ryan','a passage',1),
                ('deanna','Deanna','Alex','a passage',2);"
  fi

  # Faces are made by a model with a picture server, and there is
  # neither here, so the room is given some and some lines to put them
  # beside. Ryan and the AI have one; Alex deliberately has none, so
  # the placeholder is on the screen next to the real thing.
  if [[ $suite == avatars ]]; then
    "$bin" $(flags "$suite") >/dev/null 2>&1 &
    server=$!
    waitfor; stop
    local c
    c=$(sqlite_get "$data/room.db" "SELECT value FROM settings WHERE key='Lobby:conversation';")
    # An 8x8 PNG: a yellow square on magenta, so a screenshot shows at
    # a glance whether the picture or the placeholder won. Kept as a
    # hex literal because that is what sqlite takes a blob as.
    local face="x'89504e470d0a1a0a0000000d49484452000000080000000808020000004b6d29dc0000001b4944415478da6338a1b1002b622020f1ff8e061cd14a8204570100f6df6b312bc415490000000049454e44ae426082'"
    # A 64x64 PNG: a black diagonal on cyan, bigger than the 56-pixel
    # square Settings shows a face you have worn in. Everything a
    # picture server makes is bigger than that square -- a cel-shaded
    # portrait at 768, or a sprite handed back magnified so it reads
    # in a chat -- so that row holds both kinds at once and has to ask
    # each picture which way it is being scaled; see `lib/pixels.js`.
    local big="x'89504e470d0a1a0a0000000d4948445200000040000000400802000000250be689000000ee4944415478daeddad10d83300c0540f61faadf1dab0314151292f81ec56281930238f6dbb6ef7abd939efd8a0704197e553c20c2705c3ae00c4207441b1a0e930e083534bfd43a20ced0f971d50141864b3f391d106118d06ce800dc30ace9d301ac6170f3ad0340c3944b900ea00c132fa33a00314c1f0ae88072c3a2e18c0e28342c1d92e9801243c1b052072c36940d8d75c03243f1f05e072c30104b141d30d5002db374c02403b754d401c30de87257070c34d04b761d30c4101076d001170d31a1131dd06d080bffe8800e4364084b07341982c3703ae0a4e109253ec9d0bf88e7de21237d87a0fa9ee1030fa55e61c0ab7f510000000049454e44ae426082'"
    # Four faces Ryan has worn, the newest on: Settings offers the
    # older three back without making anything. The SQL must not start
    # with a `--` comment -- sqlite3 reads that as an option.
    sqlite_set "$data/room.db" \
      "INSERT INTO faces (who, at, png, prompt, worn)
         VALUES ('Ryan', 1699999999999, $big, 'a wizard at full size', 0),
                ('Ryan', 1700000000000, $face, 'a tired wizard', 0),
                ('Ryan', 1700000000001, $face, 'a rested wizard', 0),
                ('Ryan', 1700000000002, $face, 'a wizard at ease', 1),
                ('@Vox', 1700000000001, $face, 'a brass head', 1);
       INSERT INTO bios (who, bio, at)
         VALUES ('Ryan', 'A ranger from the north, quiet about it.', 1700000000000);
       -- Two pictures in the conversation: one that arrived and one
       -- still being drawn, so the card and the spinner are on the
       -- same screen.
       INSERT INTO pictures (id, conversation, n, who, prompt, png, failed, at)
         VALUES ('picture-one', $c, 1, 'Vox', 'a tired wizard', $face, '', 4),
                ('picture-two', $c, 2, 'Vox', 'the same wizard, rested', NULL, '', 6);
       INSERT INTO entries (conversation,id,who,kind,title,text,at)
         VALUES ($c,'a1','Ryan','human','','Make me a face, would you.',1),
                ($c,'a2','Vox','agent','','There. How is that?',2),
                ($c,'a3','Alex','human','','I will stay as I am.',3),
                ($c,'picture-one','Vox','picture','a tired wizard','picture 1',4),
                ($c,'a4','Ryan','human','','Less tired, maybe.',5),
                ($c,'picture-two','Vox','picture','the same wizard, rested','picture 2',6);"
  fi

  # A card is posted by a model, and there is no model here, so the
  # conversation is given one to show. The body is what a card is for
  # -- a table of figures, a list, a block of code -- with the two
  # things a card must never be allowed to become on the end of it.
  if [[ $suite == cards ]]; then
    "$bin" $(flags "$suite") >/dev/null 2>&1 &
    server=$!
    waitfor; stop
    local c body
    c=$(sqlite_get "$data/room.db" "SELECT value FROM settings WHERE key='Lobby:conversation';")
    body=$'**Boulder, Colorado** -- next three days.\n\n'
    body+=$'| Day | High | Low |\n|-----|-----:|----:|\n'
    body+=$'| Thu | 14\u00b0C | 2\u00b0C |\n| Fri | 9\u00b0C | -1\u00b0C |\n\n'
    body+=$'- Wind picks up *Friday* afternoon\n- Bring a coat after 6pm\n\n'
    body+=$'```\nwind = 40 km/h\n```\n\n'
    body+=$'From [the weather office](https://weather.gov/boulder).\n\n'
    body+=$'<script>alert(1)</script> <img src=x onerror=alert(2)>\n'
    body+=$'[nope](javascript:alert(3))'
    sqlite_set "$data/room.db" \
      "INSERT INTO entries (conversation,id,who,kind,title,text,at)
         VALUES ($c,'c1','Vox','card','Boulder, next three days','$body',1);"
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
[[ ${#suites[@]} -eq 0 ]] && suites=(rooms bots door permalinks ptt settings roles conversations game cards avatars drop interpreter)

bad=0
for suite in "${suites[@]}"; do
  [[ -f $here/$suite.js ]] || { echo "no suite called $suite" >&2; bad=1; continue; }
  one "$suite" || bad=1
done
[[ $bad -eq 0 ]] && echo && echo "every suite passed"
exit $bad
