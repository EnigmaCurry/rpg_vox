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

## In the room

- **When named / After every pause** (top right, shared by everyone): by
  default the AI only answers when someone says its name, so two people
  can talk to each other without it butting in. "After every pause"
  makes it answer whenever the room goes quiet.
- **Interrupting**: start talking while it speaks. It drops in volume at
  once and stops for good once your speech is confirmed (300 ms of voice
  or the first recognised word), so a cough only dips it. **Stop** does
  the same with a click.
- **Mixer** (right): a level and mute for each person and the AI, shared
  by the whole room. **Your volume** (bottom) is only yours. Space mutes
  your mic.
- **New conversation** clears the screen and the AI's memory. Old
  conversations stay in the database.

### Games

In the lobby (the room's ordinary conversation) the AI can design a game
with you. Talk it through; it fills in what you leave open, checks the
name is free, and once you've confirmed it writes the game's prompt
itself and saves it. The prompt never reaches a browser: what appears in
the chat is a card with the game's summary and an **Enter** button.

Until anyone enters it, a game is a draft: ask for changes and the AI
reads its prompt back to itself, rewrites it, and replaces the card.
Entering moves everyone into a fresh conversation run by that prompt
alone (the AI opens the scene), and fixes the game for good.

The **Games** list (right) plays any saved game from the start,
deletes it, or lets you **Peek** at its prompt (shown only to you). **Back to lobby** leaves a game; **Restart game** starts it
over. Saving needs a model with tool calling (llama.cpp needs `--jinja`);
without it the AI can talk about games but not save them.

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
