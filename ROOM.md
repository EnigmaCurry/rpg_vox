# Voice room

`vox_room` is a voice chat in the browser for a few people and an AI.
Everyone logs in with their own invite link, joins the audio with a
headset, and hears everyone else and the AI (never themselves). Each
person's speech is transcribed live into one shared conversation that
updates on every screen at once, and the AI answers out loud through
Qwen3-TTS, its words appearing as they are spoken.

It is full duplex: anyone can talk over the AI to interrupt it, with no
key to press. The conversation keeps only what the AI actually said
before it was cut off, ending with ` —`, and the AI is told where it was
interrupted.

It runs on macOS and Linux (no PipeWire: all the audio is the browsers').

## Running it

```bash
just room-invite Ryan          # prints http://127.0.0.1:7340/join/…
just room-invite Alex
export VOX_SCRIBE_LLM_MODEL=nemotron-3-nano-4b   # your llama.cpp model
export VOX_ROOM_TTS_URL=https://qwen3-tts.example.com
just room
```

Open your link, press **Join audio**, and talk. The speech-to-text models
are the ones scribe downloads (run `scribe` once if you haven't). Building
needs `pnpm` for the web app (`corepack enable` gives you one).

`just room --echo` runs without a model: the AI repeats what it heard.
Without `VOX_ROOM_TTS_URL` the voice is the local Kokoro model if scribe
has downloaded it (it does the first time it reads aloud with Kokoro),
else none: replies appear at reading pace, silently.

### Letting a friend in

Browsers only allow the microphone on HTTPS (or localhost), so put the
room behind a reverse proxy with TLS, and give it the public address so
invite links point there:

```bash
export VOX_ROOM_URL=https://room.example.com
just room-invite Alex          # https://room.example.com/join/…
```

Proxy everything, including WebSocket upgrades on `/room.ws`. The invite
link sets a cookie and goes to the room; `vox_room uninvite NAME` revokes
it and `vox_room people` lists everyone.

### In Docker

`docker/` has a compose deployment behind Traefik, with the labels
interpolated from `docker/.env` and the speech-to-text models downloaded
into a volume on first start:

```bash
cd docker
just config                    # asks about each setting, writes .env
just install
just room-invite Alex
```

See ROOM_DOCKER.md.

## In the room

- **When named / When it fits / Every pause** (top right, shared by
  everyone): who gets the turn when the room goes quiet.
  - **When named** — the AI says nothing until someone says its name, so
    two people can talk without it butting in. The default.
  - **When it fits** — at every pause the AI is *given the turn* and
    decides for itself whether the moment is its own. It is shown the
    conversation and one extra instruction: the pause between two people
    is theirs, so stay out of it unless there is a real reason to come in
    — a question you are the one to answer, a mistake worth correcting,
    something you were asked to watch for, a conversation that has run
    out. If it passes, it calls `stay_quiet` and **nothing happens at
    all**: no bubble, no "thinking", nothing on anyone's screen. It can
    also pass with `ask_again_in_seconds`, and the room shows it the
    conversation again if the quiet actually lasts that long — how it
    waits for a real silence instead of the 1.2 s gap it was shown. Only
    once per silence, so a quiet room can't loop.
  - **Every pause** — answers whenever the room goes quiet, no judgment
    involved. The cheapest, and what a game that deals the turns wants.
- **The back-and-forth**: saying its name doesn't buy you one answer, it
  starts a conversation. From then on the AI keeps the turn at every pause
  without being named again — the button moves to "When it fits" with a
  dot beside it, so it is still judging each pause rather than answering
  every one — until the room has been idle for 30 seconds
  (`--engaged-for-ms`), when it goes back to waiting for its name. You can
  also end it in words: "thanks", "goodbye", "that's all", or just turning
  to talk to someone else, and the AI stands down by itself with its
  `wait_until_named` tool. Only the mode you pick with the buttons is
  remembered; a back-and-forth is never stored, so a restart comes back to
  what you chose.
- **Interrupting**: start talking while it speaks. It drops in volume at
  once and stops for good once your speech is confirmed (300 ms of voice
  or the first recognised word), so a cough only dips it. **Stop** does
  the same with a click.
- **Mixer** (right): a level and mute for each person and the AI, shared
  by the whole room. **Your volume** (bottom) is only yours. Space mutes
  your mic, or with **Push to talk** on, your mic is open only while you
  hold Space (or the talk button): everything said in one hold is one
  entry, and the AI waits for you to let go however long you pause.
- **New conversation** clears the screen and the AI's memory. Old
  conversations stay in the database.

### Games

In the lobby (the room's ordinary conversation) the AI can design a game
with you. Talk it through; it fills in what you leave open, checks the
name is free, and once you've confirmed it writes the game's prompt
itself and saves it. The prompt never reaches a browser: what appears in
the chat is a card with the game's name and summary.

Everything else is by voice too, in the lobby: ask the AI to start a
game ("let's play Quick Draw") and, once its reply has been heard,
everyone moves into a fresh conversation run by that prompt alone (the
AI opens the scene). Ask it to change a game and it reads the prompt to
itself, rewrites it, and replaces the card; ask it to delete one and it
checks which game you mean before deleting. Inside a game the AI only
plays it: go back to the lobby to change games.

A game also carries settings, not just a prompt. Today that is
`reply_mode`, how the room is run while the game is played:

| `reply_mode` | |
|---|---|
| `room` (default) | however the people have the room set, back-and-forth and all |
| `auto` | the AI is shown every pause and judges whether to come in |
| `always` | the AI answers every pause: for a game where it deals the turns |
| `named` | the AI speaks only when a player says its name |

The AI picks one as it designs the game and can be told which you want.
A game that sets either of the last two shows it as a tag in the games
list, and while it is being played the mode buttons are locked to it;
going back to the lobby restores the mode you chose.

The **Games** list (right) plays any saved game from the start,
deletes it, or lets you **Peek** at its prompt (shown only to you). **Back to lobby** leaves a game; **Restart game** starts it
over. Saving needs a model with tool calling (llama.cpp needs `--jinja`);
without it the AI can talk about games but not save them.

### Cards, just for you

Each person has a private area at the top of the sidebar, **Just for
you**, that is on nobody else's screen. The AI deals cards into it: a
secret role, a hand, what only your character notices, an instruction
meant for you alone. It has two tools for this, in the lobby and in a
game alike:

- `list_people` — everyone on the roster, with the exact name to address
  a card to, whether they are here with a mic open, and how many cards
  they are already holding.
- `send_card` — a title and a body, to one person.

It has one more tool everywhere, `wait_until_named`, which is how it ends
a back-and-forth by itself (above).

Nothing about a card is broadcast. The server tags the event with the one
person it is for and writes it only to their own sockets, so another
browser never receives the text at all — the secrecy doesn't depend on
the page hiding anything. A card is silent, so the AI is told to say that
it sent one without saying what is on it ("Ryan, take a look at your
card"), and the sender's own voice is heard by the whole room, so it
never reads one aloud.

Cards belong to the conversation they were dealt in: **New conversation**,
**Restart game** and moving between the lobby and a game all clear
everyone's private area. A player can discard one with the **×** on it.

### What judging a pause costs

"When it fits" asks the model at every pause, so a conversation between
two people is a model call every time they stop for breath — a whole
prompt in, usually a single tool call out. That is the trade: the
judgment is a semantic one ("is this mine to answer?"), which is what a
language model is for, and asking the same model that would answer means
a pause it *does* take costs nothing extra — it just keeps talking,
with no second round trip.

If that turns out to be too expensive, the seam to replace is narrow: the
room hands out a turn with a `Cue::Decide`, and anything that can answer
"speak or stay quiet" can sit behind it — a small local model, or a
turn-taking model, without the room or the UI changing.

## How it works

- One WebSocket per tab carries 20 ms mono Opus frames both ways
  (WebCodecs in the browser, libopus on the server) and JSON room events.
- The server mixes on a 20 ms clock. For each listener it sums everyone
  except that listener, plus the AI, and encodes it for them alone.
- Each person's mic feeds its own scribe transcription engine (streaming
  text, then the offline re-decode), so their lines are labelled by who
  said them with no diarization.
- When nobody has spoken for `--reply-after-ms` (1.2 s), what was said is
  handed to the conversation, and the AI may answer. Its reply streams
  from the model a sentence at a time into the TTS (the first clause on
  its own, so the voice starts sooner), and each synthesized sentence is
  queued in the mixer.
- Every reply logs where its time went, as `reply:` lines: how long after
  the last words it was asked (`after_words_ms`), when the model's stream
  opened and its first words and first sentence arrived, how long each
  sentence took to synthesize, and when its first audio played.
- The mixer counts how much of each sentence it has played. That count,
  not the model's text, is what the conversation records: whole
  sentences, then the words of the current one that have been heard in
  full.

## Options

`vox_room --help` lists them all; each also reads an environment variable.

| Variable | Default | |
|---|---|---|
| `VOX_ROOM_LISTEN` | `127.0.0.1:7340` | address to serve on |
| `VOX_ROOM_URL` | the listen address | public address, for invite links |
| `VOX_ROOM_DATA` | `~/.local/share/vox_room` (macOS: `~/Library/Application Support/vox_room`) | the database |
| `VOX_ROOM_AGENT_NAME` | `Vox` | the AI's name |
| `VOX_ROOM_AGENT_ALIASES` | | other spellings that count as its name, comma-separated (what speech-to-text hears, e.g. `box,fox`) |
| `VOX_ROOM_REPLY_AFTER_MS` | `1200` | quiet before what was said is handed over |
| `VOX_ROOM_CONFIRM_MS` | `300` | continuous speech that cuts the AI off |
| `VOX_ROOM_ENGAGED_FOR_MS` | `30000` | idle time that ends a back-and-forth and goes back to "when named" |
| `VOX_ROOM_HISTORY` | `60` | entries of the conversation sent to the model |
| `VOX_SCRIBE_LLM_URL`, `VOX_SCRIBE_LLM_MODEL`, `VOX_SCRIBE_LLM_KEY` | as for scribe and `agent` | the model |
| `VOX_ROOM_PROMPT`, `VOX_ROOM_PROMPT_FILE` | a short one for spoken group chat | system prompt; `{name}` and `{people}` are filled in |
| `VOX_ROOM_TTS` | see above | `qwen3`, `kokoro` or `none` |
| `VOX_ROOM_TTS_URL` | `PI_TTS_URL` | Qwen3-TTS server (`/v1/audio/speech`) |
| `VOX_ROOM_VOICE`, `VOX_ROOM_INSTRUCT`, `VOX_ROOM_LANGUAGE` | `aiden`, none, `English` | Qwen3 voice, delivery and language |
| `VOX_ROOM_KOKORO_VOICE` | `am_michael` | Kokoro voice |
| `VOX_SCRIBE_MODELS` | scribe's models dir | speech-to-text models |

## Testing

`cargo test -p vox_room` covers the mixer, turn taking and barge-in.
`crates/vox_room/e2e/two-people.js` runs two headless Chromiums against a
running room, each with a WAV file as its mic, and prints what each one
hears and the conversation as it unfolds (see the top of the script).
