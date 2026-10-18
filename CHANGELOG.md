# Changelog

Changes in each release of rpg_vox, scribe, agent and Scribe for
Android. New entries go under **Unreleased**; `just publish` names that
section after the version it releases.

## Unreleased

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
