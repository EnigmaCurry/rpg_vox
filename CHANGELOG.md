# Changelog

Changes in each release of rpg_vox, scribe, agent and Scribe for
Android. New entries go under **Unreleased**; `just publish` names that
section after the version it releases.

## Unreleased

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
- **One row of controls on every screen.** Recording, playback and
  dictation each have a single row of round buttons mirrored around a
  big one in the middle, under a short status line. A ring around the
  big button shows the mic level while recording, or how far playback
  or a file's transcription has got. Recording: source and Clear, then
  record/stop, pause and Copy. Playback: stop and back 10 s, then
  play/pause, ahead 10 s and Copy. Share, Rename, Delete and the
  settings are in the ⋮ menu at the top.
- **Dictations**, a second kind of document beside threads, for
  writing by voice rather than transcribing. The drawer lists both
  together, newest first, transcriptions marked 🎙️ and dictations ✍️;
  its New button asks which to start. It edits like the voice keyboard, but the
  text is saved, so you can come back and keep going: tap the text to
  move the purple cursor where new speech goes (lowercased where it
  lands mid-sentence), select words to Delete them, and ¶ starts a new
  paragraph at the cursor. ⌨️ switches to typing with the phone's
  keyboard at the cursor; closing the keyboard comes back to dictating
  where you left off, listening again if you were.
  Threads stay as they were, for recording in order.

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
