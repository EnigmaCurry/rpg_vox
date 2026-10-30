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

Open your link, press **Join Audio** at the door, and talk. The speech-to-text models
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

Proxy everything, including WebSocket upgrades on `/room.ws` and
`/screen.ws`.

A link works **once**. Opening it spends nothing: it takes them to the
door, which says whose link it is and lets them set the name the room
will use. (That door doesn't say who is in yet — it has no cookie, and
so no socket to be told on.) **Join Audio** is what trades the link for a cookie, and from
then on that browser is who comes in — the link is dead, and anyone
opening it without the cookie is told it has expired.

`vox_room people` lists everyone, with the links nobody has used yet.
`vox_room uninvite NAME` revokes them: the link and every cookie traded
for one. The name there is the name on the invite, whatever they call
themselves in the room, and it stays theirs until they are uninvited.
`vox_room invite NAME` again, after they have joined, is a second link —
how somebody gets onto a second device, without disturbing the first.

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

- **The door**: the room is voice only, so a tab that hasn't joined the
  audio sits behind a **Join Audio** card and can't do anything else —
  everyone in the room is in it by ear. Pick your microphone and speakers
  there (both are remembered for next time), and push to talk, before you
  come in. The card also says who is already in. **Leave audio** puts you
  back at the door. The first time, on an invite link, it says *Hi,
  Mike* and offers **your name in the room**: that is the name everyone
  sees, the one beside everything you say, and the one the AI deals your
  cards to. Change it there and nowhere else — once the link is spent it
  is who you are. (The name on the invite doesn't move with it: that is
  the key `vox_room uninvite` names.) **Log out**, at the foot of the
  card, hands this browser's cookie back and forgets it — only this one,
  so another device stays in. The link it came in on is spent, so coming
  back takes a new one. Choosing a speaker needs `AudioContext.setSinkId`,
  which today means Chrome; elsewhere the room follows the system default
  and the list isn't offered.
- **The three areas**: what is yours alone on the left, the room's
  conversation on the right, the mixer beside them. On a narrow screen
  only one shows at a time, with a switch above it — **Conversation**,
  **Just for you** (with a count when you are holding cards), **In the
  room**.
- **When named / When it fits / Every pause** (top right, shared by
  everyone): who gets the turn when the room goes quiet.
  - **When named** — the AI says nothing until someone says its name, so
    two people can talk without it butting in. The default.
  - **When it fits** — at every pause the AI is *given the turn* and
    decides for itself whether the moment is its own. It is shown the
    conversation, **who is in the room**, and whether it is already
    mid-conversation, because a pause means different things in different
    rooms: with one person there is nobody else they could be talking to,
    while three people talking among themselves own the gaps between
    them. Take the turn when the last thing said was meant for it; let it
    pass otherwise. If it passes, it calls `stay_quiet` and **nothing
    happens at all**: no bubble, no "thinking", nothing on anyone's screen. It can
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
- **Typing instead of saying it**: there is a box at the foot of the
  conversation. What you send through it appears beside everything said,
  marked **typed** — the mark is the point, because a typed line has no
  recognition error in it. It is for everything speech-to-text is bound
  to spoil: a URL, a path, a command, an unusual name. The AI reads it
  like anything else, and naming the AI in one gets its attention the
  same way.

  **Typing never interrupts.** Starting to type doesn't dip the AI and
  sending doesn't cut it off, however much you send: only a voice does
  that. A message joins the conversation at the next pause, with the
  same beat as speech in which to be joined by more.

  Messages are flattened to one line and capped at 2000 characters.
  That isn't tidiness: the AI is shown the conversation as `Name: what
  they said` lines, so a message allowed to keep a line break of its own
  could put words in somebody else's mouth. Unicode's direction
  overrides and zero-width characters go the same way, for the same
  reason. Everything else survives exactly as typed, because the
  punctuation a careless filter eats is what a URL is made of.
- **More than one language**: start the room with
  `--language-also Russian` and the door offers a **Language** beside
  the devices. Everyone hears the people in the room as they are — their
  own voices, as always — and the AI in the language they picked: it
  writes its reply once, in the room's own `--language`, and a separate
  voice per language translates and speaks it. So two people can be in
  the same conversation, each hearing the AI in their own language, with
  one shared transcript. It needs `--tts qwen3`; Kokoro only speaks
  English.

  Your choice follows your invite rather than your browser, so it is
  there on your phone too, and you can change it later from the door.
  A language nobody has picked costs nothing: the AI is only translated
  while somebody is listening in it.

  A typed message is translated too, and both are kept, so coming back
  to a conversation shows what everyone was shown at the time. The link
  under it is how you get at the characters that were actually typed —
  which is what you want when the message is a URL or a command, since
  a translation is the one thing you must not copy those out of.

  Three things are worth knowing. **You hear your own language or
  nothing** — never a word of anybody else's. A translation has to be
  written before it can be spoken, so it begins about three seconds
  after the original does, and in those seconds you hear silence rather
  than the opening of the reply in a language you didn't ask for.
  (Measured on a real room: the AI's first English audio at 3.6 s, the
  same reply's first Russian audio at 6.3 s.)

  Second, **if you are listening in a translation you cannot talk the AI
  down**: starting to speak neither dips it nor cuts it, because you
  would be stopping it in the middle of something you haven't heard yet.
  **Stop** still works for you, since a click says where to cut without
  guessing.

  Third, **you read what you heard**. The AI's replies and anyone's
  typed messages appear in your language, with a **Show English** link
  under them for the words they were written in — and a **Show Russian**
  link to come back. One language at a time, with the other a click
  away: enough to catch a translation that went wrong, without two
  copies of every line to read past. Which one you are looking at is
  yours alone and changes nobody else's screen.

  The words arrive at the pace of the voice reading them *to you*. A
  translation is a different number of syllables from the words it
  translates, so each language fills in at its own rate, and a line
  translated in a tenth of a second still takes as long to read as it
  takes to say. The AI's turn is over when the last language has been
  heard out, not when the first has — otherwise ending it would take
  the rest of the reply away from whoever was still listening to it.
  Which also means a cut lands where each voice was: the dash that
  marks it is in a different place in each language.

  So that nobody has to guess at that, a reply still being spoken
  carries **a small bar per language** under it, saying how far that
  language has been heard and how much of it is left:

  ```
  ENGLISH ▬▬▬▬▬ heard    RUSSIAN ▬▬▬░░ 2s
  ```

  Everyone sees the same row, whichever language they are listening in,
  and it goes when every language has heard the reply out — which is the
  same moment the turn ends and the AI will take another. An English
  speaker can see that the Russian still has a sentence to go before
  saying anything; a Russian listener can see that the room is waiting
  for them rather than that something has stalled. A language whose
  translation hasn't come back yet says `translating…` rather than
  pretending to be at nought seconds.

  Underneath, the room's own language is still the record: it is what
  the AI is shown of the conversation, and what it is stored as having
  said, however many languages it was heard in. What people say out loud
  is not translated — only the AI's replies and what is typed.

  **Who translates.** By default the same model that writes the replies
  (`--translate openai`). `--translate local` asks Marian models on this
  machine instead — `just download-translate-model-ru` fetches the
  Russian pair, about 80 MB each — and then nothing anyone says leaves
  the room. Measured side by side on one machine
  (`cargo test -p vox_transcribe --features ct2,llm --lib -- --ignored
  --nocapture compare`):

  | | `openai` | `local` |
  |---|---|---|
  | A line | ~1900 ms | **~110 ms** |
  | Network | required | **none** |
  | Prose | best | very close |
  | A URL | kept | **kept** |
  | A command, bare | mostly kept | **mangled** |
  | A command in backticks | kept | **kept** |
  | Earlier lines for context | yes | no |

  Being seventeen times faster matters more than it sounds: it is most
  of the three-second wait above, so a translated listener hears the AI
  almost when everyone else does.

  What it costs is that a Marian model cannot be *told* anything — no
  "copy a URL through as it stands" — so left alone it renders
  `--language-also Russian` as the Russian for "also Russian language",
  and `Ctrl-C` as "2-С". So **put anything that has to arrive exactly in
  backticks**: `` `like this` ``, or a ```` ``` ```` fence. Those are
  lifted out of the line before the model sees it and put back after,
  which makes them safe whichever translator is in use. Backticks are
  worth the habit either way; with `--translate local` they are the only
  thing that works.

  Your words are written down in whatever you speak: Parakeet works out
  the language from the audio, and needs no telling. The text that
  appears *while* you are still talking is the exception — that pass
  needs a model per language (`just download-streaming-stt-model-ru`,
  or `scribe download-models --russian`), and without one your words
  simply appear a phrase later, once the sentence is decoded.

  That pass follows the language you chose at the door, so **if you
  choose one language and speak another, the text that appears as you
  talk will be an approximation of your words in the wrong alphabet.**
  It is corrected a moment later, when the sentence is decoded by the
  model that does work the language out. Nothing is lost, but it looks
  odd while it happens; choose the language you are actually going to
  speak.
- **Sharing a desktop**: **Share screen**, in the footer, offers the
  browser's own picker of a window or a screen. Everyone in the room
  gets a thumbnail of it under that person's name in the mixer, live;
  click one and it opens full size where the conversation was, with the
  mixer (and everyone else's thumbnails) still beside it. **That is
  yours alone** — opening a desktop, or closing it, changes nothing on
  anyone else's screen, and nothing about it is remembered. Several
  people can share at once; each gets a thumbnail. **Stop sharing**, the
  browser's own stop button, and **Leave audio** all end it. No sound is
  taken from what is shared: the room's audio is the mixer's, and a
  shared tab's would come back as another voice to transcribe.
- **Mixer** (right, the only thing in the sidebar): a level and mute for
  each person and the AI, shared
  by the whole room. **Your volume** (bottom) is only yours. Space mutes
  your mic, or with **Push to talk** on, your mic is open only while you
  hold Space (or the talk button): everything said in one hold is one
  entry, and the AI waits for you to let go however long you pause.

  Mute, push to talk and the mic belong to the **tab**, not to you, so
  two of your own tabs can't argue over them: you are muted once every
  tab with its mic open says so, and a tab sitting at the door — or left
  open on another screen with push to talk on — has no say in what the
  one you are speaking into does.
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
While a game that sets one of the last three is being played the mode
buttons are locked to it; going back to the lobby restores the mode you
chose. A game also records how many players it is for (`players_min` and
`players_max`, either end open), which the AI works out with you.

**Games** in the top bar is the menu of what is saved, each with its
player count. Pick one and its card opens: the name, who it is for, how
many are here now, how it runs the room, whether it has been played, and
what it is about — with **Play**, **Delete**, and **Peek** at its hidden
prompt (shown only to you). **Back to lobby** leaves a game; **Restart
game** starts it over. Saving needs a model with tool calling (llama.cpp
needs `--jinja`); without it the AI can talk about games but not save
them.

### Cards, just for you

Each person has their own cards, under **Just for you** — its own column
on a wide monitor, its own tab on a narrow one, and on nobody else's
screen. The AI deals them: a secret role, a hand, what only your
character notices, an instruction meant for you alone.

They are *cards*, and the prompt tells the AI to call them that out loud
— "Ryan, check your cards" — because the obvious name for a region of
somebody's screen is a phrase no voice should ever say to a room.

What is in that area is the AI's to decide, and only the AI's — there is
no way for the person holding a card to clear it. The four tools, in the
lobby and in a game alike:

- `list_people` — everyone on the roster, with the exact name to address
  a card to, whether they are here with a mic open, and the titles of
  what each is already holding.
- `send_card` — a title and a body, to one person. Sending a title they
  already hold **rewrites that card where it sits**, which is how a hand
  or a standing order changes without stacking copies; the card glows
  again so they notice, and the footer says it changed rather than
  arrived.
- `take_back_card` — removes one by title, once it no longer applies.
- `read_cards` — what it has given out, in full. A long game outruns the
  conversation the model can still see, and a game master that has
  forgotten who the traitor is cannot run the game.
- `post_card` — the opposite: a card written into the **conversation**,
  for the whole room, which stays there and cannot be taken back. It is
  where anything everyone should be looking at belongs — the rules as
  agreed, the standings, the cast of a scene — and it saves the AI
  reading a list aloud. It is shown to the model as its own, in full, so
  it keeps playing by what the room can see.

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
everyone holding nothing. Short of that, a card stays until the AI takes
it back.

### What a shared desktop costs

The thumbnail is small (320 px wide, 2 frames a second, VP8) and goes to
everyone all the time, which is what makes the mixer live rather than a
list of names. The full size is only *encoded* while somebody has it
open: the relay tells the sharer's browser whether anybody is looking,
and with nobody looking it isn't made at all.

The server never decodes any of it. It forwards frames and keeps one
rule — **a viewer is sent no delta frames until a keyframe has reached
them** — which is what makes dropping safe: a viewer who falls behind is
put back to the next keyframe instead of being shown a torn picture, and
the sharer is asked for one. That is also how somebody who arrives
mid-stream fills in.

It is a second WebSocket (`/screen.ws`), and the separate connection is
the point: a burst of video in the audio socket's send queue would be
head-of-line blocking applied to speech. Proxy it like `/room.ws`.

What this does not have is WebRTC's congestion control. The rates are
fixed, so a desktop that is mostly still costs little and a bad network
degrades without a feedback loop to degrade *gracefully*. If that ever
matters more than the simplicity does, the whole of it is `screen.rs`
and `web/src/lib/screen.svelte.js`, and nothing in the audio path knows
it exists.

Sharing needs WebCodecs video (`VideoEncoder`), so the button is only
offered where the browser has it. Watching needs `VideoDecoder`.

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

The judgment is only as good as what the model is told, and the failure
is silent: a model that is told a pause is probably not its own says
nothing, for ever, and the mode looks broken rather than wrong. There is
a test for it that puts real pauses to the configured endpoint and checks
which way it goes, off by default because it costs what that endpoint
costs:

```bash
cargo test -p vox_room -- --ignored --nocapture judges
```

Run it after changing anything the model reads. There is a second live
test, `whole_reply`, that puts a real reply through the whole path — the
room's own request, the stream parsed as it arrives, a tool run from it,
and the model carrying on from the tool's answer.

## How it works

- One WebSocket per tab carries 20 ms mono Opus frames both ways
  (WebCodecs in the browser, libopus on the server) and JSON room events.
  A tab sharing or watching a desktop has a second one, `/screen.ws`,
  that carries VP8 frames and nothing else.
- The server mixes on a 20 ms clock. For each listener it sums everyone
  except that listener, plus the AI in that listener's language, and
  encodes it for them alone. The AI's languages are alternatives rather
  than parts of a mix: one queue each, and you hear exactly one of them.
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

### Thinking, and which API

Two different things used to share one name, so they now have two.

`--reasoning none|low|medium|high` (default `low`) is how hard the model
may think before it answers. Note the default: before, thinking was off
unless you asked for it, and now a little of it is on. `--reasoning none`
is the old off. On a local server it turns the chat
template's thinking on or off (`chat_template_kwargs.enable_thinking`,
which is a llama.cpp and vLLM convention and nothing else knows it); on
OpenAI it is the reasoning effort. `--think` is the old name for
`--reasoning medium`, and `VOX_AGENT_THINK` is still read — leniently,
since as a flag anything but exactly `true` or `false` used to stop the
room from starting.

`--llm-api auto|responses|chat` is which API to speak. It matters
because **OpenAI's reasoning models refuse function tools on
`/chat/completions`** unless the thinking is switched off entirely — and
the room puts tools on every reply, for the cards and the games — so on
that API the AI runs every game, and judges every pause, without
thinking. `/responses` takes both together. A local llama.cpp or vLLM has
only `/chat/completions`.

`auto` asks `/responses` first and remembers, once per run, if the
endpoint hasn't got one, so neither kind needs configuring. Against
OpenAI it is also the faster of the two: thinking at `low` through
`/responses` beat thinking switched off through `/chat/completions`, which
pays for a rejected request before every reply.

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
| `VOX_ROOM_REASONING` | `low` | how hard it thinks: `none`, `low`, `medium`, `high` |
| `VOX_ROOM_LLM_API` | `auto` | which API: `auto`, `responses`, `chat` |
| `VOX_ROOM_PROMPT`, `VOX_ROOM_PROMPT_FILE` | a short one for spoken group chat | system prompt; `{name}` and `{people}` are filled in |
| `VOX_ROOM_TTS` | see above | `qwen3`, `kokoro` or `none` |
| `VOX_ROOM_TTS_URL` | `PI_TTS_URL` | Qwen3-TTS server (`/v1/audio/speech`) |
| `VOX_ROOM_VOICE`, `VOX_ROOM_INSTRUCT`, `VOX_ROOM_LANGUAGE` | `aiden`, none, `English` | Qwen3 voice, delivery and language |
| `VOX_ROOM_LANGUAGES` | | other languages the door offers, comma-separated and spelled out in English (`Russian`). The AI answers in each one somebody is listening in; needs `--tts qwen3` |
| `VOX_ROOM_TRANSLATE` | `openai` | who translates: `openai` asks the reply model, `local` asks Marian models on the CPU (see below) |
| `VOX_ROOM_KOKORO_VOICE` | `am_michael` | Kokoro voice |
| `VOX_SCRIBE_MODELS` | scribe's models dir | speech-to-text models |

## Testing

`cargo test -p vox_room` covers the mixer, turn taking and barge-in.
`crates/vox_room/e2e/two-people.js` runs two headless Chromiums against a
running room, each with a WAV file as its mic, and prints what each one
hears and the conversation as it unfolds (see the top of the script).
`e2e/share-screen.js` is the same idea for desktop sharing: one shares,
the other watches, and it reads the pixels off each canvas to check the
picture really arrived — and that opening it was one person's doing
alone. `e2e/hear-out.js` needs no microphone at all: two tabs on
different languages, a typed question, and it reads the per-language row
off both screens to check that one language is shown heard out while the
other is still going, that both tabs are shown the same thing, and that
the row goes when the turn does.
