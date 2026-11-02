# Changelog

Changes in each release of scribe, Scribe for Android, Scribe for iOS,
agent, Pi voice, Voice room and rpg_vox. New entries go under
**Unreleased**; `just publish` names that section after the version it
releases.

## Unreleased

### scribe, Scribe for Android

- **A language can now be read in yours even when it is written the
  same way.** French into English was refused outright, and so was
  Russian with Ukrainian, because a paragraph's language was worked out
  from its alphabet and nothing else -- the recognizer that hears 25
  languages never says which one it heard. The alphabet was never wrong
  about Cyrillic against Latin, and could say nothing at all about Latin
  against Latin, so the honest thing was to refuse the pair rather than
  send a paragraph to a model that would translate it out of a language
  it was not in and return confident nonsense.

  The alphabet still goes first, still costs nothing and is still never
  overruled when it answers. What it could not settle now goes to
  fastText's `lid.176`: 917 KB for 176 languages, pure Rust, asked only
  about the languages you turned on and only above nine-tenths
  certainty. Under that the paragraph stands as it was said, which is
  what used to happen to every one of them.

  Nine-tenths is measured and not a round number. Over Flores-200 and
  over real decodes from this repo's own recordings it is where the
  wrong answers stop: Czech against Slovak against Polish is 2.2% wrong
  with no floor and 0.04% at this one, and the one English paragraph
  that was called Spanish is called Spanish at every floor up to 0.8
  and refused at 0.9. `examples/langid_bench.rs` is what took the
  numbers and `just langid-bench` takes them again.

  For Scribe for Android, reading in English, that turns five offered
  languages -- never more than two of them on at once, one per alphabet
  -- into twenty-one, with no limit on how many. For `scribe` it means
  `--other-language` may share an alphabet with yours or with another of
  them; without the classifier the old refusal stands, rather than a
  guess.

### Scribe for Android

- **Models are a page of their own.** Downloading them was a section of
  the Languages page that had grown until it was most of it, and it
  could only ever offer what the language settings happened to want.
  **Settings → Models** is now every model: what each is for, how big
  it is, whether it is on the phone, and a button to fetch or delete
  it, with the total on disk at the top. A row says whether a model is
  in use, unused, or wanted by a chosen language and missing -- which
  is the state in which nothing is being read, and which previously
  looked like a recording that simply would not translate.

  The Languages page keeps the three things it is for: the language you
  read in, the languages to listen out for, and what a paragraph shows
  while its reading catches up. Nothing on it downloads anything, and
  the rule that turning on one language turned off whichever other one
  shared its alphabet is gone with the restriction behind it.

- **A reading says that it is a reading, and will show you the line it
  was made from.** A translated paragraph was the same black text as
  one somebody actually said, with nothing to tell them apart and no
  way to see what the translator had been given. It now carries a rule
  down its left edge -- the whole of the indicator, present on readings
  and on nothing else -- and over it the language it was heard in,
  **From English**.

  Tapping that opens the thread sentence by sentence: each sentence as
  it was said, faintly, with its reading on the line beneath it. Zipped
  rather than the reading followed by the paragraph, because checking a
  reading means holding one sentence against one sentence, and down a
  long paragraph two blocks are too far apart to do it. A sentence the
  translator hasn't reached stands alone; tapping a reading plays from
  where its own sentence was spoken, as before.

  One switch for the whole transcript and not one per paragraph: any
  tag throws it, they all show the same chevron, and one tap puts the
  thread back to the readings alone. Reading a translated thread
  against the original is a way of reading, not a note taken on one
  paragraph. A paragraph nothing has been read of stays closed however
  the switch is set -- there is nothing to compare it against yet, so
  its tag is a label with no chevron.

  Distinct from **Show what was said**, which is about the sentence a
  reading has not reached yet. This is about the ones it has. The
  switch is not stored: it is how the transcript is being read just
  now, and it starts closed.

### Voice room

- **More than one AI in a room, and one pause between them.**
  `--bot NAME[:VOICE[:PROMPT_FILE]]` (`VOX_ROOM_BOTS`) puts another AI
  in every room. Each has its own name to be called by, its own voice,
  its own prompt with `{name}` filled in as itself, its own fader under
  the room, and its own **Answers** setting per room -- so one of them
  can answer every pause in your own room while waiting to be named in
  the Lobby.

  **Who answers a pause** is settled by who speaks first. Every bot
  that could answer is given the turn at once and the first to actually
  say something keeps it; the rest are told to stop. Asking them one at
  a time would put a round trip of silence in the room for every bot
  with nothing to say, and letting them all finish would be two voices
  over each other. A bot cut off this way leaves nothing behind -- no
  bubble on anyone's screen, nothing in the conversation, and nothing
  for the model to read back later as a turn where somebody said
  nothing. A bot already speaking is never cut off by another starting:
  it is the first word that claims the pause, and it has already had
  it.

  Nothing inside the room assumes there is one AI any more. A card is
  signed by the bot that dealt it, a game's button by the bot that
  designed it, a failed reply names the bot whose turn it was, and
  nobody in the room may call themselves after any of them. The screen
  follows: each strip says what *that* bot is doing, **Stop** names
  whichever of them is talking, and the prose that used to say "the AI"
  says all their names.

- **One server, several rooms, and a room of your own.** A `vox_room`
  used to be a room. It is now a house: the rooms you name with
  `--room` (`VOX_ROOM_ROOMS`), which everyone shares, and on top of
  those one of your own for everybody -- made the first time you walk
  into it, admitting nobody else, and where the AI is yours alone.

  **Moving doesn't cost you the audio.** You call in once and stay
  called in. The socket, the mic frames going up and the mix coming
  back are all exactly where they were; what moves is which room's
  mixer the far end is plugged into, whose conversation your screen is
  showing and where your voice is being transcribed.

  The rooms are a list down the left, flush to the edge, one line each
  with a count of who is in it. **Your own room is first and stays
  first**, then the house's in the order they were named -- the one
  room that is always there and always the same is the one to put
  where the eye lands. The room you are **in** opens underneath itself
  into everybody there -- the faders and meters and mutes that used to be a
  panel on the right. The mixer is a branch of the room it mixes now,
  which is what it always was: a fader belongs to the room it is being
  heard in, and the room you are not in has none you could usefully
  touch. A list rather than a row of buttons because a server can hold
  a great many rooms, and a row of buttons cannot.

  **Every room has an address**, and the bar follows you from one to
  another: `/` is the room of your own (per person, so whoever opens it
  gets theirs), `/lobby` is the first room named with `--room`, and
  `/room/<id>` is every other one. Back and forward move between
  rooms, and a link pasted to somebody opens that room for them. The
  id is not the room's name, because a name is a thing people change
  their minds about and a link that changed with it is a link somebody
  has already sent on. An address that names no room quietly becomes
  the address of the room you ended up in.

  **A game is something a room does.** It is played in a room, by the
  people in it, and while it runs the game is what that room is --
  with the room's own name still in the heading and the game beside
  it, and the list of rooms saying which room has one on.

  And leaving a game now **puts the room back where it was**. What the
  room was talking about is put aside rather than thrown away, and
  *Leave the game* picks it up again -- every word of it, and every
  card anyone was holding -- for everyone in the room at once. It used
  to start a third empty conversation, so a game cost you the one you
  were having to get into it and another to get out. A game is an
  interruption; the room goes on afterwards. (*Restart game* is not
  finishing with one, so what it interrupted is still there.)

  **A room keeps the conversations it has had.** *New conversation*
  closed the one you were having and that was the end of it: it stayed
  in the database, unreachable. It is the room's history now, and
  **History** at the head of the conversation is the list — when each
  was, how much was said, and its opening line, newest first with the
  live one marked. Open one and you read it as it was, cards and
  translations and all, with no box to type into: the room has moved
  on from it, so read-only is what it is rather than a rule about it.

  **A conversation can be called something.** The name at the head of
  it is a box: type over it and that is what the room calls this
  conversation, for everyone in it and in the history from then on. It
  names whichever one you have open, an old one included.
  Left alone it is when it was — the time of the first thing said, or
  the day it was started if nothing has been — and emptying the box
  gives it back to the clock rather than leaving it blank. The room
  keeps the name and the numbers; how a date reads is each browser's
  own business, in its own language and timezone.

  What the AI is shown is who is in the room too: `list_people` says it
  lists who is in the room, and it was listing everybody invited with a
  flag beside them — which is how it came to deal a card to somebody
  two doors down. Addressing one to somebody who has just stepped out
  still works, and the card waits where they left it.

  A conversation nobody said anything in is not one the room had, and
  isn't on the list — pressing *New conversation* twice leaves nothing
  behind the second time. The one the room is having now is always
  there, empty or not, because that is where you are.

  Reading one is **yours alone**. Nobody else's screen moves, which is
  the only way it could work — a room where one person looking
  something up pulled everyone else out of the live conversation would
  be a room nobody dared look anything up in. Deleting one is the
  exception: it takes that conversation, everything said in it and
  every card dealt in it away from everybody, for good, and asks
  first.

  Conversations are written down with the room that had them, from the
  moment they start, which is what makes any of this possible. A
  database from before this keeps each room's *current* conversation;
  the ones before it can't be told apart by room, so they are left out
  of every room's history rather than guessed into one.

  **The door is a page, and coming in is something you agree to.** The
  Join Audio card used to be a sheet over a blurred room: the
  conversation was there to read, and the sockets were open, before
  anybody had joined anything. It is its own page now, with nothing
  behind it and nothing open — a tab at the door is not on a roster,
  not sent a conversation, and not counted as being anywhere, and who
  is in a room is no longer something the door tells you.

  And the card now says what coming in does: your microphone goes live
  to everyone else, what you say is transcribed and written into a
  conversation the room keeps, and that writing is sent on to the AI
  and to the translation and voice services the operator has
  configured. **Join Audio** does nothing until you agree. The answer
  is kept in the browser, so it is asked once, and unticking it takes
  it back.

  **The door's Language box always has a language in it.** It was blank
  for anybody the room already knew: the box's first option carried the
  empty string the room stores for its own language, and what the room
  answers with is the name. Every option is its own name now. The
  default is whatever the system is set to, when the room speaks it,
  and the room's own otherwise — a guess made once and written down in
  the browser, so it can never afterwards overrule a choice.

  **The interpreter is yours now, and only there when it is working.**
  It used to be one fader in the room's mixer, shown whenever the room
  was *configured* to interpret and shared by everyone in it -- so
  turning the voice down turned it down for people who needed it, and
  a room where everybody spoke the same language still carried the
  strip. An interpretation is heard by whoever is listening in that
  language and by nobody else, so it is theirs: the fader is per
  listener, the duck follows it per listener, and the strip appears
  only while somebody in the room is speaking a language you don't
  read. A room of your own never has one at all -- the AI is already
  answering in your language, and there is nobody in there to
  interpret to -- and no longer starts the threads and voices for one.

  **The top bar is the room you are in, and what you press.** "Vox is
  listening" has gone — what the AI is doing is on the AI's own strip
  in the room list. The heading is the room's name: the list says it
  too, but only on a screen wide enough to show the list, and on a
  narrow one it is behind a tab.

  **When the AI answers now lives under the AI**, in the room list, as
  that bot's setting in that room -- which is the shape a second bot in
  a room would need. And the Games menu has gone from the top bar: a
  game is announced in the conversation that designed it, and that card
  is where **Play** and **Peek** are, which is one place to look
  instead of two.

  The default room is called **Lobby**. That word already had a job
  here -- "the lobby" was the room being in no game -- so the button
  that leaves a game now says **Leave the game**, which is what it
  does and what it always did; a button reading "back to the lobby"
  beside a room called Lobby would be a button that lies.

  Each room is a whole room: its own conversation, its own faders, its
  own mode, its own games and cards. Walk out of one and back into it
  and it is as you left it. What is the person's rather than the
  room's -- the invite list, the saved games, your name, the language
  you hear -- is shared across all of them, so choosing a language at
  the door still follows you everywhere.

  A room of your own has a roster of one and a mixer with you and the
  AI on it. Nothing said in it reaches the house, and nothing said in
  the house reaches it. Because nobody is there to take the turn from,
  it starts out answering every pause instead of waiting to be named --
  having to say "Vox" to a room with only the two of you in it is a
  ceremony with no point to it. Change it and it remembers.

  **You are in a room or you are not.** The list under a room is who
  is there, and that is the whole of it: somebody invited but in
  another room is on that room's count, not greyed out as "away" on
  this one. The roster used to be everyone invited with a flag, which
  made "away" stand for two unrelated things — gone, and standing two
  doors down — and neither of them is a thing this room has any
  business claiming about a person. What they speak is still known
  wherever they are, so a line they said before they walked out is
  still read in your language.

  The strips lost their boxes and a third of their height along the
  way, since a column that has to hold the rooms as well cannot spend
  eighty pixels a person. Everything is still there: the meter, the
  fader, the mute. The decibel reading is the one thing that comes and
  goes — it appears while the fader is off unity, which is exactly when
  you are looking for it, and the fader gets its width back the rest of
  the time.

  Two other things came with all this. A desktop is shared with the
  room you are in and no other, and the share goes with you when you
  leave. And the clocks -- the 20 ms mixer frame and the 50 ms
  turn-taking tick -- are the server's now rather than each room's: a
  thread per room waking fifty times a second to find nobody in it is a
  poor way to hold an empty room open.

  Each room keeps its conversation, its mode and its faders in the
  database under its own name, so renaming a room starts it fresh and
  the old one's conversation waits there for the name to come back. An
  existing database has one room in it, and its things become the first
  `--room`'s the first time the new version starts.

- **"Just for you" is gone when there is nothing in it.** Half the
  window used to be given over to a column saying what might one day
  appear in it -- a secret role, a hand, what only you noticed -- and
  for most of a conversation that is all it ever said. Now the area is
  only there while you are holding a card: no cards, no column, and the
  conversation has the half back. The first one dealt opens it; the
  last one taken back closes it again. On a narrow screen the tab comes
  and goes with it, and if the last card is taken back while you are
  looking at that tab you are put back on the conversation rather than
  left on an empty one.

  Nothing about the cards themselves changed: they are still the AI's
  to give, rewrite and take back, and still on nobody else's screen.

## v0.2.0 (2026-11-01)

### scribe

- **A paragraph's sentences are translated in one call, not one call
  each.** `--translate local` ran the model once a sentence, so a
  paragraph that pass 2 hands over three sentences at a time cost three
  decodes. They go to CTranslate2 as one batch now, which is one decode
  over the three -- the same work the model does anyway, done once. The
  reading was keeping up on a desktop and falling behind on a phone,
  where the recognizers already hold the cores; this is the first thing
  to try there. `Translate::translate_all` is where a translator says it
  can take lines together: the default still asks line by line, which is
  what `--translate openai` wants (nothing to batch when the cost is the
  round trip, and it reads the lines before each one for agreement), and
  a run of new sentences is broken wherever one already read sits
  between them, so the context each line is given is what it would have
  been asked one at a time. Ten sentences of a real transcript, through
  the en-ru model: 1047 ms a call a sentence against 329 ms as one
  batch, with every reading identical -- which the test asserts, since
  batching is meant to change the cost and nothing else. Greedy search
  (`beam_size` 1) would take that to 234 ms and is not taken: it
  changes eight readings in ten, some for the better and some for the
  worse, which is a poor trade for a third off something that is
  already a third of what it was.

- **`--translate`: what was said in another language, read in yours.**
  Pass 2's recognizer hears twenty-five languages without being told
  which, but it never says which it heard — so the alphabet is read off
  the text, and a paragraph that comes back in Cyrillic is also put into
  English, a sentence at a time. What was said is never replaced: the
  reading goes under it, as a blockquote in the markdown, in its own
  colour in the TUI, and as `NAME.translated.srt` beside the subtitles of
  what was said. `--translate local` is Marian on the CPU with no
  network at all (about 50 ms a sentence, needs a build with the
  `translate-local` feature); `--translate openai` asks the `--llm`
  service and can be told to leave a command alone. `--native-language`
  is what you read in, `--other-language` what to listen out for. The
  alphabet is the whole of the detection and the limit is stated rather
  than papered over: a pair that shares one — German against English — is
  refused at startup, because guessing there means translating a line out
  of a language it was never in. See
  [translation](SCRIBE.md#translation-optional).

- **Every `just` recipe that builds scribe for this machine can
  translate locally.** `just scribe`, `just release-scribe` and
  `just install-scribe` all carry the `translate-local` feature now, so
  they are one build between them rather than three variants, and the
  binary in `~/.local/bin` is the one `just scribe-translate` runs
  rather than one that answers `--translate local` with "needs a build
  with the feature". `install-scribe` then has the installed binary
  fetch its own models, which lands them in the per-user directory an
  installed scribe resolves to rather than the checkout's.

- **`--translate local` downloads its model, like every other model.**
  It was the one thing in scribe you had to put in place by hand: a
  `just` recipe, a Python with `sentencepiece` in it, and the models
  copied to wherever the binary you were running resolves them. Now the
  first `--translate local` fetches the direction it is about to read
  (~80 MB) into the models directory it resolved, and says so while it
  does — `scribe download-models --translate` takes the same download in
  advance, `--both-ways` adds the reverse of each, and `just
  install-scribe` copies nothing anywhere any more.

  What stood in the way is that nobody publishes a `tokenizer.json` for
  opus-mt, and the SentencePiece library that reads the `.spm` they do
  publish cannot be in the same binary as sherpa-onnx — their protobufs
  are different versions of the same symbols. So the conversion is in
  Rust now (`vox_transcribe::spm`): a `.spm` is a protobuf, the pieces
  and their scores are two fields of it, and the pure-Rust `tokenizers`
  crate that reads the result can also write it. It produces the same
  bytes `tools/spm2tokenizer.py` did, which is checked rather than
  hoped for — that script keeps its place as the one that can compare
  against a real SentencePiece, and the bytes it wrote are fingerprinted
  in a test.

- **The release archives translate locally too, Windows included.**
  `just dist` builds scribe with `translate-local` on every target, so
  the Linux, macOS and Windows archives all answer `--translate local`
  rather than "needs a build with the feature"; iOS is the one left
  without. The models are not in the archive — 80 MB a direction — and
  do not need to be: an unpacked archive downloads the one it is asked
  for, as above. Three things had to agree before Windows would link it:
  the C runtime (sherpa-onnx's prebuilt archive and CTranslate2 are both
  built against the static one, while the C++ bridge between them took
  cc's default, and the linker refuses that pair), the CPU dispatch
  (CTranslate2 reads `__AVX__` to decide which copy of its kernels it is
  compiling, so the `-mavx` that libopus needs has to stay out of its
  C++, or the generic kernels the dispatcher falls back to are never
  built), and, for a cross-build only, what a library file is called:
  ct2rs looks for `lib*.a` whenever its build script is *running* on
  Unix, whatever it is building for, so `just win64` names CMake's
  `.lib` files twice over and runs the build script again.

- **Playback was in the left ear only.** On PipeWire, everything scribe
  plays — `--live`, `--play`, and `--chat` replies read aloud — came out
  of one speaker. The stream is fed mono samples and said so, which gets
  it a single port whose channel position is `MONO`; the session manager
  links ports by position, so `output_MONO` matched `playback_FL` and
  nothing at all was linked to `playback_FR`. Nor was there an upmix to
  be had, since the adapter had negotiated the one channel that was
  asked for. The stream now opens as stereo with its channels named and
  writes each sample to both, which is what the cpal backend (macOS,
  ALSA) always did with however many channels the device has. Exactly
  the capture-side bug fixed below, on the other side of the graph.

- **The translation keeps up with the speech instead of waiting for the
  paragraph.** A sentence is read as soon as it is finished, not when
  the paragraph it is in settles — which, at up to two hundred words a
  paragraph, could be a minute after the first thing somebody said. It
  is refined as the paragraph forms, too: pass 3 rewrites words at
  utterance boundaries and pass 4 rewrites whole paragraphs, and a
  sentence that changed is read again while every sentence that didn't
  is reused, because the sentence *is* the cache key. That is what makes
  it free: a fifteen-minute file now translated in thirteen passes over
  one paragraph takes the same wall clock, to within a second, and
  produces byte-identical output. The reading deliberately runs one
  sentence behind the words on screen — a translator given half a
  sentence returns half a thought — and shows a trailing `…` while it
  does. Nothing is written to disk half-translated: a paragraph never
  hardens on a reading that is short of its text.

- **`--pass1-language`: live text in the language being spoken.** Pass 1
  is the one pass that has to be told which language it is hearing —
  passes 2 and 3 work it out from the audio and the translation works it
  out from their text, but a streaming model has to be loaded before
  anything has been said. There has been a Russian pass-1 model all
  along; scribe could only *download* it, for `vox_room`, and always
  loaded the English one itself, so Russian speech showed ALL CAPS
  English nonsense until each utterance was decoded. Now
  `--pass1-language Russian` loads it (fetched on first use, ~28 MB) and
  the live text is Russian. It is a choice, not a detection: one engine
  loads one model, so a language this isn't set to still gets the
  nonsense — `--pass1 ellipsis` and `--no-streaming` are the ways to
  want neither. Pass-1 text is upper-cased for the screen now rather
  than relying on the English model's own tokens being upper case, so
  "dim ALL CAPS means still settling" holds in both languages.

- **Words no longer come out of order.** A short utterance that the
  live pass never showed (too quick for it) only reached the transcript
  when its re-decode finished, and was added after whatever had been said
  since: a word or phrase from the start of a sentence could turn up at
  its end, or, on a file, paragraphs later. It now goes where it was said.

- **`--jsonl`: dictation over a pipe.** Another program (an editor, a
  chat client) can start `scribe --jsonl`, read what you say as JSON
  lines on stdout (`partial` text while it is still settling, `final`
  once a paragraph is done) and send `finish`, `cancel`, `delete_word`,
  `set_text` or `ping` on stdin. `finish` runs every pass on what was heard and sends the rest
  before exiting, `delete_word` takes back the last word said, even
  one a later pass is still rewriting, and `set_text` hands over text
  edited by hand for new speech to follow. With `--standby` it stays
  running between dictations, mic released, so the models load once:
  `start` opens the mic, `finish` / `cancel` release it, `pause` and
  `resume` release and reopen it mid-dictation, `reset` clears the
  text. `--auto-finish SECONDS` (or `start`'s `auto_finish_ms`)
  finishes a dictation by itself once you stop talking. stdout carries nothing else; logs
  stay on stderr. See
  the [reference](SCRIBE_REFERENCE.md#--jsonl-dictation-over-a-pipe).

- **Every channel of a multichannel input is heard now.** On PipeWire,
  capture asked for a single mono channel and let the graph sort it out;
  the session manager links ports by channel position, so a lone `MONO`
  port on a stereo source matched `FL` and left `FR` unlinked. Half the
  room was silently dropped — a stereo device transcribed only its left
  channel. The stream now opens in the source's own channel layout and
  mixes it down itself.

- **One channel per speaker**: `-d <device>:FL` transcribes a single
  channel of a multichannel input instead of all of them mixed, so two
  mics on one stereo receiver become two speakers, `-d rx:FL -d rx:FR`.
  `:FR`, `:L`, `:right` and `:ch3`… also work. `scribe devices` lists the
  channels of anything with more than one, and the `a` menu offers them
  as rows of their own.

- **`y` copies the whole transcript** in the recording TUI (and
  `--once`), unless a paragraph is selected with `↑`/`↓` — then it
  copies just that row. `End` clears the selection to get the whole
  text back. It used to copy the newest paragraph when nothing was
  selected.

- **A reading arrives with the utterance it is of, not the one after
  it.** A sentence counted as finished only when there was whitespace
  after its full stop. That is right for the paragraph as it stands --
  there the last sentence is where the speaker has got to, and
  translating it would translate half a thought -- and wrong for the
  text pass 2 has settled, where the speaker *ended* that sentence and
  the clip closed. Holding it back waited for the next utterance, which
  is a silence away rather than a sentence away: long gaps between
  readings, and on a phone held through a monologue it looked like
  translation only happened when the recording stopped. The live pass
  now reads the settled text up to its last finished sentence
  (`language::complete_settled`), where a full stop at the very end
  counts and only an unfinished tail is held. A test fails if the first
  reading arrives any later than the first utterance.

### Scribe for Android

- **The app itself is in English or Russian.** Every word it says --
  buttons, settings, dialogs, notifications, the voice keyboard, what
  TalkBack reads out -- was a literal in Kotlin, which is to say the
  app spoke one language and had no way to be told otherwise. They are
  resources now, in `values/strings.xml` with `values-ru/` beside it,
  and the app follows the phone's language list; on Android 13 and
  newer `locales_config.xml` also gets it a **Language** entry of its
  own in system Settings, so it can be read in Russian on an English
  phone. Which language the app is in and which language it
  *transcribes* are separate, and were worth keeping separate: a
  Russian reader transcribing English wants one of each. Three things
  stay in English on purpose -- the changelog, which is CHANGELOG.md as
  it was written; a model's directory name, which is what the release
  it came from is called; and the engine's own errors, which a bug
  report wants quoted. Nothing already recorded is rewritten: a
  paragraph's speaker is still stored as `Me` or `Phone` and a reading
  still remembers `heard_in` by the engine's English name, so switching
  the language changes the words on the screen and not a byte of the
  thread. Russian numbers properly, too -- "перевод отстаёт на 2
  абзаца" against "на 5 абзацев", which is four plural forms where
  English has two. `StringsTest` holds a translation to the same names,
  the same format arguments and enough plural forms for its language,
  and fails the build when one drifts; [ANDROID.md](ANDROID.md#translating)
  says what adding a third language costs, which is a file and a line
  of XML.

- **The status line says when the reading has fallen behind.** It used
  to degrade in silence: recording from another app, the words kept
  arriving and the readings quietly stopped keeping up, with nothing to
  say whether the translator was behind, broken or switched off.
  Android gives the app you are looking at the fast cores and an app in
  the background the slow ones — measured on a Pixel 6, that is pass 2
  running a third slower and the reading at about half the speed of the
  speech — so "Listening 4:21 · reading 3 paragraphs behind" is now part
  of the status line, counting the finished paragraphs still waiting.
  The paragraph being spoken is never one of them: a reading runs a
  sentence behind on purpose. See
  [ANDROID.md](ANDROID.md#recording) for what to do about it, which is
  mostly to leave Scribe on screen.

- **A reopened translated paragraph plays from the sentence you tap.**
  The reading was stored as one piece, so a thread opened again had
  nothing to say which sentence was which: every tap on it played from
  the start of the recording. The sentences are kept now, each with the
  one it was made from and when that one was spoken
  (`messages.lines`, the shape the engine hands them over in), so a
  reopened paragraph taps the way a live one does. Paragraphs stored
  before the column keep the whole reading against the whole
  paragraph -- the alignment they were saved without cannot be worked
  out after the fact.

- **What was said shows until its reading lands, in the language it was
  heard in.** A reading arrives a sentence behind the speaking, and the
  sentence being spoken used to be a faint `…` -- a paragraph being
  heard in another language was an ellipsis and nothing else until the
  translator had read its first sentence. The words themselves stand
  there now, faintly, in the language they were heard in, each giving
  way to its reading as that lands, so the screen shows that something
  is being heard and roughly how much. **Settings -> Languages -> While
  a reading catches up -> Show what was said** turns it off for the
  ellipsis again.
  A reading is still what a translated paragraph is, and still what
  Copy, Share and the notification say of one: this is what the place a
  reading has not taken yet looks like.

- **Settings -> Languages says it in lines rather than paragraphs.**
  The page had grown three paragraphs of explanation around four
  choices, and the last toggle's description was six lines long with
  the switch floating somewhere in the middle of it. It fits on one
  screen now, with a heading that says what each part is for -- **I
  read in** ("All text will be written and/or translated in English"),
  **Choose secondary languages (one per alphabet)**, **Manage
  dictionaries** for what is to be downloaded or deleted, and **While a
  reading catches up**. The alphabet rule is in the heading rather than
  repeated under every greyed-out language, a leftover dictionary is
  named in languages ("English -> Russian") rather than in the
  directory it is kept in ("en-ru"), and what the page used to spell
  out is in [ANDROID.md](ANDROID.md#languages), where there is room for
  it. About says what the app is in one line: live transcription,
  translation and dictation on the phone, with local models.

- **A new thread is named for when you spoke, not when you made it.**
  Making a thread and then waiting to press record left the title
  stamped with the wait rather than the recording. A new thread is now
  named with today's date alone, and gains the time when recording
  starts -- so long as the title is still the app's and nothing has been
  heard in the thread yet. The title says whose it is (`auto_title` on
  `conversations`, which the desktop ignores) instead of being
  recognised by its shape: renaming a thread hands it over for good, and
  reusing an untouched new thread rather than piling empty ones up no
  longer rests on a regular expression matching the date.

- **A long recording no longer fills memory with transcripts the screen
  has not caught up with.** Every paragraph event carries the whole
  paragraph, which for a paragraph minutes long is megabytes of JSON,
  and a recording with two sources resends each piece of it on every
  update. When the main thread fell behind, each `poll` still handed it
  one more copy, and the engine's queue kept every update in between:
  after a quarter of an hour the app stopped answering and then died
  trying to turn a 290 MB backlog into one Java string. A drain now
  collapses the updates of a paragraph to its latest state, as it always
  did for the level meter, and a poller waits for the main thread to
  finish the batch before rather than handing it another.

- **It reads another language in yours, on the phone.** The translation
  the desktop got is in the app, with CTranslate2 built for the NDK, so
  a paragraph heard in Russian also reads in English with nothing asked
  of a network. **Settings → Languages** has the three choices it takes:
  the language you *read in* (what every translation is read into), the
  language the *live text* is in, the languages to *listen out for* (a
  model each, 81 MB, downloaded there), and what the transcript *shows*
  of a paragraph that was read.

  The transcript is in the language you read and in no other. A
  paragraph heard in Russian is shown in English, a sentence at a time
  as each is read, and the Russian is not shown at all -- there is no
  setting for it, because a transcript you can read is the point of
  translating one. What was said is still what is kept: it is in the
  thread, it is what every later pass refines, and it is what each
  reading is made from again when one of those passes changes a word, so
  a sentence pass 3 rewrites is read again. Each line of a reading
  carries the sentence it was made from (`TranslatedLine`) to keep the
  two together through all of that. A sentence not yet read shows as an
  ellipsis -- the reading is a sentence behind the speaking on purpose --
  and tapping one of a translated paragraph's sentences plays from where
  that sentence was spoken, a translation having no word timings of its
  own. Copy, Share and the notification say what the screen says.

  What is shown as what was said is now what is *not known to be* in
  another alphabet, which is two fixes in one rule. A paragraph pass 1
  is still typing has no verdict at all, and a paragraph whose alphabet
  could not be read -- too few letters, or too much of a mixture, which
  `<unk>` and a stray word manage easily -- gets the same verdict as one
  in your own language: nothing to translate. Both were shown as what
  was said, which for Russian is Russian. The app asks the alphabet
  itself now, by the same rule and the same numbers the engine uses
  (`script`, a majority of at least eight letters), and shows what was
  said unless the answer is an alphabet you don't read.

  Text too short or too mixed to place is still shown -- "OK." is as
  likely as "Да.", and hiding every short utterance to catch the second
  would lose the first -- and the engine is told to assume the language
  pass 1 is listening in for exactly those, so most of them are read
  rather than left to that judgement. Only when the alphabet says
  nothing at all: a line that is readable and in none of the languages
  being listened out for is a line in your own, and sending that to a
  translator would get it back unchanged.

  The words that appear *as you speak* come from pass 1, the one pass
  that has to be told which language to listen in -- it decodes as the
  words are said, so its model is chosen before any of them have been,
  where passes 2 and 3 work the language out from the audio. Nobody is
  asked which it will be: pass 1 starts in the language you read in and
  follows the speaking, and an utterance that came back in another
  alphabet is enough for the next one to be listened to in that
  language. It is the alphabet pass 2 heard that decides, the same
  reading the translation itself goes by, and it changes between
  utterances and never within one -- a decode swapped mid-utterance
  would lose the words already on screen. The model for a language you
  turn on is downloaded with it (27 MB for Russian), and until it
  arrives the settings page and the recording status line say the live
  text cannot be in that language yet, rather than listening in the
  wrong one without a word about it.

  The languages offered are the ones that can be told apart. A
  paragraph's language is read off its alphabet, since the recognizer
  that hears 25 of them never says which it heard, so a language written
  in the alphabet you read could not be told from your own and two
  written in each other's could not be told apart: turning one on turns
  off whichever other one shares its alphabet. What was said is always
  what is stored, and the only text with word timings, so playback,
  editing and cutting stay with it — "The reading" hides it rather than
  replacing it. Readings are saved with the transcript, in two columns
  the desktop's chat database ignores, so a thread reopens with them.

- **The command-line bundle translates too.** `just android`, the
  `dist/android-arm64/` bundle for a shell on the phone, is built with
  `translate-local` like everything else, so `--translate local` works
  there as it does on the desktop. It wants `curl` to fetch the model,
  which an Android shell may not have; `adb push` a models directory, or
  point `VOX_SCRIBE_MODELS` at one, if it hasn't.

  Both Android builds now ship `libc++_shared.so`: CTranslate2 is the
  first C++ of ours in them, and the NDK links the shared C++ runtime
  where sherpa-onnx's prebuilt libraries carry a static one. Leaving it
  out is a crash at startup on the phone and nothing at all at build
  time, so the APK's libraries are now checked against what they say
  they need, the way the bundle's already were -- which is how this was
  caught without a device.

- **The app downloaded the wrong Parakeet.** It fetched
  `parakeet-tdt-0.6b-v2` while the library had moved to v3, so a fresh
  install downloaded 487 MB and then failed to load with the file it was
  looking for missing. One side of that rename had been left behind.

### Voice room

- **First release: a voice chat in the browser for a few people and an
  AI.** Everyone joins with an invite link and a headset, hears everyone
  else and the AI, and reads one live transcript of who said what. The
  server mixes on a 20 ms clock and encodes a separate mix-minus for
  each tab, so nobody hears themselves — Opus over one WebSocket,
  WebCodecs in the browser. Each person's mic feeds its own
  `vox_transcribe` engine, which is what lets a line be labelled with
  who said it. The AI answers through any OpenAI-compatible model and
  reads its replies aloud with Qwen3-TTS (Kokoro or silent as
  fallbacks). Talk over it and it drops in volume at once, then is cut
  once the speech is confirmed: the log keeps only what the mixer
  actually played, ending in a dash, and the model is told where it was
  interrupted. No PipeWire, so it runs on macOS and Linux. See
  [ROOM.md](ROOM.md).

- **Everyone hears the AI in their own language.** `--language-also
  Russian` puts a **Language** on the door, and the AI writes its reply
  once, in the room's own `--language`, while a separate voice per
  language translates and speaks it. Two people are in the same
  conversation, each hearing the AI in their own language, over one
  shared transcript; the people are always heard as they are, in their
  own voices. The choice follows your invite rather than your browser,
  so it is there on your phone too, and a language nobody has picked
  costs nothing, since the AI is only translated while somebody is
  listening in it. Needs `--tts qwen3` — Kokoro only speaks English.

  Three things are stated rather than papered over. **You hear your own
  language or nothing**: a translation has to be written before it can
  be spoken, so it starts about three seconds after the original and
  those seconds are silence rather than the opening of the reply in a
  language you didn't ask for (measured on a real room: the first
  English audio at 3.6 s, the same reply's first Russian at 6.3 s).
  **If you are listening in a translation you cannot talk the AI down**,
  because you would be stopping it in the middle of something you
  haven't heard yet — **Stop** still works, a click saying where to cut
  without guessing. And **you read what you heard**: replies and typed
  messages appear in your language with a **Show English** link under
  them for the words they were written in, one language at a time with
  the other a click away, and which one you are looking at is yours
  alone.

  The words arrive at the pace of the voice reading them *to you*. A
  translation is a different number of syllables from the words it
  translates, so `Reply` keeps a `Vec<Said>` per language and
  `Mixer::ai_progress` reports every queue rather than only the
  canonical one. The turn ends when the *last* language has been heard
  out, not the first — ending it with the English took the rest of the
  reply away from whoever was still listening to it — and a reply still
  being spoken carries a bar per language under it, `ENGLISH heard
  RUSSIAN 2s`, so each side of the room can see it is being waited for
  rather than that something has stalled. Each voice says for itself
  when it is done, since an empty queue cannot tell a voice that has
  finished from one whose next sentence is still being translated.
  There is a thirty-second backstop with a warning, for the one case it
  is there for: a voice thread dying would otherwise hold that turn open
  for ever, and with it every turn after it.

- **People are interpreted too, not only the AI.** What anyone says out
  loud is said again, by a separate voice, to everybody in the room who
  doesn't speak it — the way a booth at a conference does it. The
  per-listener mix became `(everyone - you) - (1 - env) * foreign + ai +
  interpreter`, so only the voices actually being interpreted duck; the
  ones you can already follow stay exactly where they were, nothing
  being said over those. Pass 2 is the trigger, which is the first point
  there is a finished sentence rather than a guess that is still
  changing, and puts the interpreter about half a second behind the end
  of an utterance (45 ms to translate on the CPU, ~390 ms to
  synthesize). Pass 3 rewrites clips afterwards and is deliberately
  never spoken again: by then the words have been heard, and nobody can
  unhear them. One thread per language, each with its own voice, so what
  was said is heard in the order it was said;
  `--interpreter-voice` defaults to `ryan` rather than the AI's `aiden`,
  because the one thing a listener has to be able to tell by ear is
  which of the two is talking.

  Every language goes through one `@interpreter` strip, and how far the
  floor drops under it is that fader: `0.2^gain`, a straight line in
  decibels at about 14 dB of duck per unit. At unity it is the duck that
  was always there; at the top the interpreted voices drop 28 dB; at
  nothing there is no duck at all, which is what makes pulling it down
  *off* rather than a hole in the room. Down means off down to the cost,
  since the interpreter asks the mixer before paying a voice for
  anything — but the queues still drain and the reading is untouched,
  because taking a person's transcript away because somebody else found
  the voice intrusive is not a trade one slider should make.
  `--interpret false` is still there for a room that wants none of it.

  An interpreter with more than eight seconds still to say stops paying
  for a voice and only writes: another sentence on the end of that queue
  would be heard later still, and the words have been on screen the
  whole time, so being read in the present beats being heard in the
  past. The decision is per utterance and per language, so a queue that
  drains goes back to speaking by itself. Nothing is dropped quietly any
  more either — a reader given nothing has no way of telling that
  anything was skipped — so a failed interpretation shows what there is
  to show: the translation when only the voice failed, and the line as
  it was actually said when the translating is what failed. What makes
  an interpreter fall behind is not the translating but having nowhere
  to put the audio; `the interpreter is behind` and `the interpreter
  cannot keep up` are those two limits from outside.

- **`--translate local`: the room's translation on this machine.**
  Marian opus-mt through CTranslate2, about 110 ms a line against the
  reply model's 1900 ms, and nothing anyone says leaves the process.
  Both translators sit behind one `Translate` trait, so the room holds
  an `Arc<dyn Translate>` and neither the voice threads nor the typists
  know which one they have. A Marian model cannot be *told* anything,
  which is the one real cost — no "copy this URL through as it stands" —
  so the trait carries `needs_backticks` and `translate::protect` lifts
  backticked spans and fences out of a line before the model sees it,
  putting them back after.

  The tokenizer is the awkward part: SentencePiece and sherpa-onnx each
  bundle protobuf-lite, and statically linked that is two copies of the
  same symbols. The image's way out is to not have SentencePiece at all
  — `tools/spm2tokenizer.py` converts `source.spm` to a pure-Rust
  `tokenizer.json` in a build stage that has Python for exactly that
  long, and refuses to write one that splits any of seven probe lines
  differently from the `.spm`, so a wrong conversion fails the build
  rather than the room.

- **The AI judges whether a pause is its turn.** "After every pause"
  became three modes. **When named** is the default and says nothing
  until the AI is named, so two people can talk without it butting in.
  **Every pause** answers whenever the room goes quiet, no judgment
  involved — the cheapest, and what a game that deals the turns wants.
  In the new middle one, **when it fits**, the room hands the AI the
  turn with a `Cue::Decide` and it answers or calls `stay_quiet`, so
  three people can talk with it in the room without it taking every gap
  between them. A turn it passes on leaves nothing behind at all: no
  bubble, no "thinking" flashing every 1.2 s. Passing with
  `ask_again_in_seconds` has the room show it the conversation again if
  the quiet actually lasts, once per silence so a quiet room can't loop.
  The decision is the same model that would answer, with a tool to
  decline, so a pause it takes costs no second round trip and only
  passes cost a prompt.

  What it is told is the whole of it, and getting that wrong is silent:
  the note used to presume the answer, so with one person in the room —
  nobody else they could be talking to — the AI stayed out of that
  conversation too, and the mode looked broken rather than wrong. It now
  carries what the room knows and the model cannot guess: who is here,
  and whether a back-and-forth is already on. There is a live test
  against the configured endpoint, off by default because it costs what
  that endpoint costs.

- **Games the AI designs with the players, and cards it deals.** In the
  lobby the model gets tools to list games, check a name is free, save
  one once the players have confirmed it, read and revise one nobody has
  played yet, and start or delete it by voice. A saved game is a card in
  the chat; entering starts a fresh conversation run by that prompt
  alone, with the AI opening the scene. The prompt never leaves the
  server: **Peek** fetches it from `/api/games/NAME/prompt` for the
  person who asked and never over the room's broadcast.

  The cards are information the AI chose to give one person, so they are
  the AI's to keep: there is no discard, and dealing a title someone
  already holds rewrites that card where it sits — a hand that changes
  as it is played, rather than five copies of "Your hand". `read_cards`
  gives them back in full, because a long game outruns the conversation
  the model can still see and a game master who has forgotten who the
  traitor is cannot run the game. `post_card` is the opposite: one
  written into the conversation where everyone reads it and it stays,
  for what the room should be looking at rather than holding in their
  heads. A posted card cannot be taken back — it is part of what the
  room has been told, like anything that was said.

  The AI's every word is read aloud, and the prompt used to teach it to
  say "check your private area", which does not mean in speech what it
  means on a screen. It has the words now — "Ryan, check your cards" —
  and a test keeps the other phrasing out.

- **Sharing a desktop.** **Share screen** puts a thumbnail in the mixer
  — 320 px wide, 2 frames a second, VP8 — which is what makes the mixer
  live rather than a list of names; the full size is only *encoded*
  while somebody has it open, the relay telling the sharer's browser
  whether anybody is looking. The server never decodes any of it. It
  forwards frames and keeps one rule — a viewer is sent no delta frames
  until a keyframe has reached them — which is what makes dropping safe:
  a viewer who falls behind is put back to the next keyframe instead of
  being shown a torn picture, and that is also how somebody arriving
  mid-stream fills in. It is a second WebSocket (`/screen.ws`), and the
  separate connection is the point, a burst of video in the audio
  socket's send queue being head-of-line blocking applied to speech.
  What it has not got is WebRTC's congestion control: the rates are
  fixed, so a still desktop costs little and a bad network degrades
  without a feedback loop to degrade *gracefully*. Needs WebCodecs
  video, so the button is only offered where the browser has it.

- **Typing instead of saying it.** A box at the foot of the
  conversation, for everything speech-to-text is bound to spoil — a URL,
  a path, a command, an unusual name. What you send appears beside
  everything said, marked **typed**, and the mark is the point, because a
  typed line has no recognition error in it. **Typing never
  interrupts**: starting to type doesn't dip the AI and sending doesn't
  cut it off, however much you send, since only a voice does that.
  Messages are flattened to one line and capped at 2000 characters, and
  that isn't tidiness — the AI is shown the conversation as `Name: what
  they said` lines, so a message allowed to keep a line break could put
  words in somebody else's mouth. Unicode's direction overrides and
  zero-width characters go the same way, for the same reason, and
  everything else survives exactly as typed, because the punctuation a
  careless filter eats is what a URL is made of.

- **An invite link works once, and the door is where it is spent.** The
  link used to *be* the credential: it was the cookie, reusable by
  anyone it was ever forwarded to, and `uninvite` left every browser
  already holding one no worse off than before. It is a one-time token
  now, traded for a cookie of the browser's own when **Join Audio** is
  pressed, in one `UPDATE ... WHERE used = 0` so two browsers racing the
  same link can't both win; `uninvite` takes both. Opening the link
  spends nothing — it goes to the door, which says *Hi, Mike* and offers
  the name the room will call them by. That name is what the transcript,
  the strips and the cards are keyed by, which is why the field is only
  on an unspent invite: changing it later would orphan everything
  addressed to them. **Log out**, at the foot of the card, hands this
  browser's cookie back, and only that session goes, so another device
  stays in. An existing database migrates in place: the old cookie *was*
  the invite token, so each becomes a session and every browser already
  in stays in.

- **A door in front of the room, and three areas instead of a sidebar.**
  Joining the audio was a button in the footer among the mixer controls,
  easy to miss and easy to sit behind; it is a card in front of the room
  now, which stays visible but inert until you have joined, so everyone
  in the room is in it by ear. The microphone, the speakers and push to
  talk are chosen there — the decisions that belong before you come in —
  and the card says who is already in. On a wide screen what is yours
  alone is on the left and the room's conversation on the right, the
  same size, with the mixer beside them; a narrow one shows one of the
  three at a time, with a switch above it that counts the cards you are
  holding. The games left the sidebar for a menu in the top bar, where
  picking one opens its card: who it is for, how many are here now, how
  it runs the room, whether it has been played, and **Play**, **Delete**
  and **Peek**.

- **Push to talk, and muted meaning not heard.** With push to talk on
  the mic stays muted except while Space or the talk button is held, and
  stays open 300 ms after release so the last word gets through; the
  setting is remembered per browser, and one hold is one entry, the room
  not handing over until it is let go. A mic muted from the bottom bar
  or the mixer is no longer transcribed or taken for talking over the
  AI, so the AI coming back through speakers can't interrupt it, and
  **No headset** turns on the browser's echo cancellation for people
  playing the room out loud.

- **`--llm-api` and `--reasoning`: it can think and use tools at once.**
  OpenAI's reasoning models refuse function tools on
  `/chat/completions` unless the thinking is off, and the room puts
  tools on every reply for the cards and the games — so every game was
  being run, and every pause judged, with reasoning switched off.
  `/responses` takes both, and one request loop now drives either API:
  the body, the streamed events, the way a tool call comes back and the
  way its answer goes in. `--llm-api auto` asks `/responses` first and
  remembers once per run if the endpoint hasn't got one, which is what a
  local llama.cpp or vLLM will say. The two things that were both called
  thinking are now apart: `--reasoning none|low|medium|high` is how hard
  it thinks, whichever API that means, and `--llm-api` is which API
  (`--think` stays as the old name for `--reasoning medium`, and
  `VOX_AGENT_THINK` is read by hand now, so an old `.env` setting it to
  `0` or `no` doesn't stop the room from starting).

- **In Docker.** `docker/` is a self-contained deployment behind
  Traefik, with the labels interpolated from `docker/.env` and the
  speech-to-text models downloaded into a volume on first start. It
  assumes nothing about where the Docker daemon is — everything is a
  named volume, so a remote context works the same as a local one.
  `just config` walks `.env.example` and asks about every key,
  `just install` builds and starts, and `room-invite` / `room-uninvite`
  / `room-people` run the room's CLI inside the container against the
  same SQLite file the running room has open. `docker/.env` is
  deliberately *not* the repository's own. See
  [ROOM_DOCKER.md](ROOM_DOCKER.md).

### Pi voice

- **First release: a voice conversation with the Pi coding agent.**
  `pi/scribe-voice.ts` drives `scribe --jsonl --standby` for what you
  say and `pi/pi-tts.py` for reading the replies aloud, paced to the
  voice and shown as they are spoken; talking over a reply cuts it off.
  `just pi` runs it and `just pi-tts-venv` sets up the voice. The voice
  is Qwen3-TTS through mlx-audio on a Mac, or, with `PI_TTS_URL`, a
  vllm-omni server (rpg_vox's qwen3-tts stack), which is what makes it
  work from Linux. See [PI.md](PI.md).

## v0.1.10 (2026-10-18)

### scribe

- **Windows (x86_64).** scribe and agent now build and run on Windows,
  and each release has a `.zip` of them for `x86_64-pc-windows-msvc`
  beside the macOS and Linux archives. It carries the two C runtime
  DLLs as well, so the folder it unpacks to needs nothing installed.
  Use it from Windows Terminal.
  - The mic, the speakers and the device list come from WASAPI, through
    cpal as on macOS.
  - **System audio without a virtual device**: an output device opened
    for input hands back what it is playing, so `-d loopback` (the
    default output) or `-d "<output device>"` transcribes a call, a
    video or anything else coming out of the speakers. `scribe devices`
    lists them and marks which ones work that way, and the TUI's `a`
    menu offers them. That is the job `--virtual-sink` does on PipeWire
    and `--app` on macOS, neither of which Windows has: WASAPI can't
    single out one app.
  - Models live in `%LOCALAPPDATA%\vox_scribe\models`, the clipboard
    is written through Win32 (`clip.exe` would mangle everything that
    isn't ASCII), and sherpa-onnx's C++ log lines are kept off the TUI
    the same way as on unix, by swapping descriptor 2.
  - Not available there: `--app`, `--virtual-sink`,
    `--tts speech-dispatcher` and `scribe stop-once`. Each says so
    rather than misbehaving. `--once` itself records, copies and exits
    as it does elsewhere; what it loses is being finished by a second
    press of the key that started it, which signals the first process.
- **`scribe download-models` unpacks the model archives itself** instead
  of running `tar`, so nothing has to be installed for it. It skips
  anything in them that isn't a plain file or a directory.

## v0.1.9 (2026-10-15)

### scribe

- **Sentence starts are capitalized.** The recognizers punctuate text
  they don't re-case, so a sentence break pass 3 found inside a
  re-decoded window came out as "... in Hell. the surrounding land ...".
  The first letter after a `.`, `!` or `?` is now capitalized in passes
  2 and 3 — but not after `3.5`, `etc.` or a trailing-off `...` — and
  pass 3 no longer drops the capital from a clip that opens a sentence
  its window began on. rpg_vox's transcription does the same.

- **`--paragraph-gap SECONDS`** (env `VOX_SCRIBE_PARAGRAPH_GAP`) sets the
  quiet that ends a paragraph in auto mode, until now a fixed 2 s. It is
  the same wait a paragraph takes to settle and be written, manual ones
  included, so a shorter gap also gets text into the `-o` file sooner.
  `--chat` still starts from 0.3 s; passing the option overrides that
  too.
- **`--paragraph-soft-words` and `--paragraph-hard-words`** (env
  `VOX_SCRIBE_PARAGRAPH_SOFT_WORDS`, `VOX_SCRIBE_PARAGRAPH_HARD_WORDS`)
  set the word counts that end a paragraph, until now a fixed 100 at the
  next pause and 200 regardless. `0` turns a cap off, as `--chat` does
  with both.
- **A word cap now holds while the re-decode lags.** A clip joins its
  paragraph as soon as the live pass opens it, but the caps were only
  checked when pass 2 landed, so a paragraph swallowed every utterance
  the decoder was behind on — every utterance of an `--input` file,
  which is pushed faster than real time, leaving one 103-word paragraph
  where a 15-word cap was asked for. rpg_vox's transcription and Scribe
  for Android get the same fix.

## v0.1.8 (2026-10-15)

### scribe

- **speech-dispatcher reads `--chat` replies**, in whatever voices the
  desktop's own speech daemon is set up with, so a machine where speech
  already works chats without the 350 MB Kokoro download. `--tts`
  (env `VOX_SCRIBE_TTS`) picks the engine: the default asks
  speech-dispatcher and falls back to Kokoro when it doesn't answer —
  and only asks, leaving a daemon that isn't running alone rather than
  starting one.
  - `scribe voices` lists the names `--voices` and `v` take for the
    engine that would speak; speech-dispatcher's are `module:voice`
    (`pocket-tts:vera`, `espeak-ng:English (America)+Alex`), for the
    module and language the daemon is set to.
  - Where the module reports index marks (espeak-ng does, pocket-tts
    doesn't) the reply's words light up as they are really spoken;
    where it doesn't they are spread over the reply by length, timed
    from the replies spoken so far.
  - Reading a transcript aloud (`-p NAME` with no `NAME.opus`) stays
    Kokoro's: that player seeks and changes speed through the samples,
    and speech-dispatcher plays its speech itself.
- **Speech no longer comes out choppy on PipeWire.** The playback
  stream filled each buffer the server handed it (12288 frames, 256 ms
  here) instead of the frames the graph asked for (one 256-frame
  quantum), so every cycle drained the 200 ms ring dry and padded the
  rest with silence: in a measured two seconds of audio, 560 ms came
  out as holes. It now fills `Buffer::requested()`, and the ring holds
  500 ms so the fallback for servers that ask for nothing in
  particular still fits a whole buffer. Affects everything scribe
  plays — Kokoro replies in `--chat`, `--play` (a recording or a
  transcript read aloud) and `--live`. The same mistake, and the same
  fix, as rpg_vox's source stream.
- **A chat needs a responder.** `--chat NAME` waits a few seconds for
  one to say it is listening and then exits with the commands that
  start one, rather than opening a conversation that would never be
  answered. `agent` and `scribe chat-echo` say so in `NAME.db` every
  couple of seconds; `--no-responder` opens the chat regardless, for a
  responder of your own.
- **Dictation deletes by the word, or by the character.** `backspace`
  takes the word before the cursor and `del` the word after it (where
  `del` used to only delete the selection); `alt+backspace` and
  `alt+del` take a single character, like a notepad. `backspace` no
  longer takes the whole sentence — that moved to `shift+backspace`
  (or `ctrl+backspace`), which only terminals speaking the kitty
  keyboard protocol can tell from a plain `backspace`.
- **`e` opens a dictation for typing**, beside the `tab` and `i` that
  already did.

### agent

- **It checks the model answers before taking the conversation.** One
  request at startup, so a wrong URL, a missing API key, a server that
  isn't running or a model it hasn't got exits 1 with the server's own
  message — instead of surfacing mid-conversation as "Sorry, I couldn't
  get an answer just now." Reasoning models that reject `max_tokens`
  are asked again with `max_completion_tokens`, then with neither.
- **It says it is listening** in the chat database every couple of
  seconds, which is what `scribe --chat` now waits for at startup.

## v0.1.7 (2026-10-13)

### scribe

- **PulseAudio systems.** On Linux without PipeWire running, scribe
  records from the mic and plays back through ALSA, which PulseAudio
  routes as usual. Capturing an app (`--app`) and `--virtual-sink`
  still need PipeWire. `VOX_AUDIO_BACKEND=alsa` or `pipewire` picks one
  without checking.

## v0.1.6 (2026-10-13)

### scribe

- **Dictation**, as in Scribe for Android: `scribe --dictate NAME`
  writes `NAME.md` by voice and keeps it, so you can come back and keep
  going.
  - The arrow keys or a click move the cursor where new speech goes
    (lowercased where it lands mid-sentence); a click below the text
    puts it at the end.
  - `enter` starts a paragraph at the cursor. `backspace` deletes the
    sentence before the cursor, the paragraph break right before it,
    or the selection if there is one. Select with `shift` and the
    arrows, or by dragging.
  - `tab` opens the text for typing by hand at the cursor; `esc` goes
    back to dictating there, listening again if you were.
  - `space` starts and stops listening, with no pause. `y` copies the
    selection, or the whole text.

## v0.1.5 (2026-10-12)

### scribe

- Builds without a deprecation warning on the newest Rust toolchains.

### Scribe for Android

- **App icon**: the rpg_vox logo (the hooded figure with the mic), in
  place of Android's default.

## v0.1.4 (2026-10-12)

### scribe

- **Pass 2 keeps up with background music.** The voice detector's
  threshold now rises with the background level (2.5× the quietest
  moment of the last 3 seconds), so pauses between your words count as
  silence even with music or a fan on. Before, steady background kept
  every utterance open until the 12 second cap, delaying the re-decode.
  Quiet rooms are unaffected.
- **Pass 2 doesn't wait on sound without words.** When the live
  transcription (pass 1) has heard no new word for 1.5 seconds, the
  utterance closes even if it's still loud, and pass 2 runs on it.
- **`--denoise` (experimental)** runs the DeepFilterNet 3 noise filter
  on the audio before transcribing it, to push background music down
  and keep the voice. `--denoise-limit DB` caps how much it removes.
  While it's on, `◆ denoise` shows beside the level meter (which then
  reads the filtered level).
- **`--pass1 off|text|ellipsis|hidden`** chooses how the live pass-1
  text is shown: as now, as a run of `…` that grows with each word
  heard, not at all while pass 1 still runs (`hidden`), or not at all
  with pass 1 skipped (`off`). Either way text appears after pass 2.
  The colour legend under the meter follows it.

### Scribe for Android

- The voice keyboard shares scribe's engine, so it gets the same two
  background-music fixes: pauses count as silence over steady
  background, and audio with no new words closes after 1.5 seconds.
- **Dictations**, a second kind of thread beside transcriptions, for
  writing by voice rather than recording in order. Each is saved, so
  you can come back and keep going.
  - Tap the text to move the purple cursor where new speech goes
    (lowercased where it lands mid-sentence); a tap below the text puts
    it at the end. It works on text from earlier sessions too.
  - ¶ starts a paragraph at the cursor. ⌫ deletes the sentence before
    the cursor, the paragraph break right before it, or the selection
    if there is one. Selected words can also be deleted from the
    selection menu, across paragraphs.
  - ⌨ opens your keyboard to edit the text by hand, starting at the
    cursor. Closing the keyboard goes back to dictating where you left
    off, listening again if you were. The controls hide while the
    keyboard is up, and the text keeps the cursor in view as it opens
    and closes.
  - Record and stop is all it needs; there's no pause. Leaving the app
    stops listening and saves.
  - The voice keyboard and dictations share the same editing code. A
    cursor move or ¶ made while the keyboard is paused now stays where
    it was made.
- **One list of threads.** The drawer lists dictations (✍️) and
  transcriptions (🎙️) together, most recently changed first. **+ New**
  asks which to start, Dictation first.
- **Start recording on New** (Settings › Recording, off by default): a
  new dictation or transcription starts listening as soon as it opens.
- **One row of controls on every screen**, under a short status line:
  four round icon buttons mirrored around a big one in the middle,
  whose ring shows the mic level while recording, or how far playback
  or a file's transcription has got.
  - Dictation: ¶, ⌫, dictate/stop, ⌨, Copy.
  - Transcription: source (Mic, Phone or Both), Clear, record/stop,
    pause, Copy.
  - Playback: stop, back 10 s, play/pause, ahead 10 s, Copy.
  - Share, Rename, Delete and the settings pages move to a ⋮ menu at
    the top (it replaces ⚙). The separate level bar, source picker,
    playback bar and Copy/Share links are gone.

## v0.1.3 (2026-10-11)

### Scribe for Android

- **Voice keyboard: Pause.** Pause stops transcribing without losing
  what you have; the mic stays open so Resume picks up at once.
- **Keeps going when hidden.** Switching apps no longer throws the
  dictation away: it keeps listening (unless paused), so you can switch
  to read something out, and the text is there when you come back.
- **Purple cursor.** It marks where new speech goes. Tap between words
  to move it; what you say next goes there. Capitals and full stops
  where speech lands mid-sentence are fixed up ("a Brown dog." becomes
  "a brown dog").
- **Delete and Edit** in the text selection menu. Edit pauses and
  retypes the selected words on a small built-in keyboard.
- **¶ button** for a paragraph break at the cursor; otherwise the text
  is one paragraph, with no automatic breaks.
- The transcript follows the cursor as it grows. While the speech models
  load, the keyboard shows "Loading models…".
- Buttons are ⌨ (back to your other keyboard, discarding the dictation),
  ¶, Pause and Done (formerly Insert), all the same width.

## v0.1.2 (2026-10-08)

### Scribe for Android

- **Keep audio.** A Recording setting saves each recording (.opus) with
  its transcript. Tap any word to play from there: the words light up as
  they're spoken and the transcript follows along.
- **Playback controls:** back and ahead 10 seconds, pause and resume
  (also by tapping the transcript), and stop. They're in the
  notification and on the lock screen too, and playback pauses for calls
  and when headphones are unplugged.
- A thread plays straight through all its recordings, with a play button
  above each one; long-press it to delete that recording.
- **Keep non-speech audio** (off by default): recordings in which no
  words were heard are kept and shown as "No words detected". With it
  off, they're deleted, and playback shortens quiet between words to one
  second.
- **Edit and Cut** in the text selection menu: retype a paragraph, or
  remove words along with their stretch of audio. Cut also works while
  recording.
- Paragraphs stay in the order they were said, even when one is
  finished after a later one.
- Transcribing a file shows how far it has got and the time left.
- Settings are split into Microphone, Recording, Keyboard and Display
  (font size) pages, plus an About page with this changelog.
- Clear asks first, and Rename opens with the keyboard up.
- The ¶ button is gone; paragraphs break on pauses.
- Install and update with [Obtainium](ANDROID.md#install).

## v0.1.1 (2026-10-07)

### Scribe for Android

- First release: live transcription on the phone with the same models
  as scribe, from the mic, other apps' audio, or both; shared audio and
  video files; threads; Bluetooth headset mics and mic boost; recording
  in the background; and a voice typing keyboard. The APK is attached to
  each release.

### scribe

- Spoken digit strings are written as numerals.
- `scribe --chat` talks with an LLM through the new agent, with
  conversations kept as threads.
- An Android arm64 build of the command-line scribe (`just android`).

## v0.1.0 (2026-10-05)

- First release: scribe for macOS and Linux, and rpg_vox for Linux.
