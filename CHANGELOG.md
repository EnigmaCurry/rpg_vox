# Changelog

Changes in each release of rpg_vox, scribe, agent and Scribe for
Android. New entries go under **Unreleased**; `just publish` names that
section after the version it releases.

## Unreleased

### scribe

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

### Scribe for Android

- **It reads another language in yours, on the phone.** The translation
  the desktop got is in the app, with CTranslate2 built for the NDK, so
  a paragraph heard in Russian also reads in English with nothing asked
  of a network. **Settings → Languages** has the three choices it takes:
  the language you *read in* (what every translation is read into), the
  language the *live text* is in, the languages to *listen out for* (a
  model each, 81 MB, downloaded there), and what the transcript *shows*
  of a paragraph that was read: what was said, the reading, or both.

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
