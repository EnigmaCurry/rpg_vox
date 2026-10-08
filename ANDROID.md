# Scribe for Android

[scribe](SCRIBE.md)'s live transcription as an Android app. Words
appear as they are spoken and are corrected when you pause, using the
same speech models and passes as the desktop version, all running on
the phone. It also adds a voice keyboard for dictating into other apps.

* **On the phone, offline.** After a one-time model download, nothing
  leaves the device.
* **Mic, phone audio, or both.** Transcribe yourself, whatever another
  app is playing (a call, a video), or both at once with each paragraph
  labelled Me or Phone.
* **Files.** Share an audio or video file to Scribe, or open one with
  it, and it is transcribed faster than real time.
* **Threads.** Each transcript is a thread you can rename, reopen and
  add to later.
* **Keeps going in the background.** Recording continues with the
  screen off, with Pause/Resume and Stop in the notification.
* **Voice typing.** A Scribe keyboard that listens as soon as it opens
  and types the finished text into any app.

Arm64 phones running Android 8.0 (API 26) or newer. English only.

* [Install](#install)
* [First run: speech models](#first-run-speech-models)
* [Recording](#recording)
* [Threads](#threads)
* [Playing back](#playing-back)
* [Transcribing a file](#transcribing-a-file)
* [Settings](#settings)
* [Voice typing keyboard](#voice-typing-keyboard)
* [Building from source](#building-from-source)

## Install

### With Obtainium (recommended)

[Obtainium](https://github.com/ImranR98/Obtainium) installs apps
straight from their GitHub releases and updates them when a new one is
published.

1. Install Obtainium.
2. On the phone, open
   [Add Scribe to Obtainium](https://apps.obtainium.imranr.dev/redirect?r=obtainium://add/https://github.com/EnigmaCurry/rpg_vox),
   or tap **Add App** in Obtainium and enter
   `https://github.com/EnigmaCurry/rpg_vox`.
3. Tap **Add**, then **Install**. Android asks you to allow installs
   from Obtainium the first time.

Each release also carries the desktop scribe archives, but Obtainium
only looks at the `.apk`.

### By hand

Download `scribe-<version>-android-arm64.apk` from the
[releases page](https://github.com/EnigmaCurry/rpg_vox/releases) on the
phone and open it. Android asks you to allow installs from your browser
or file manager the first time. Repeat for each new version.

If a new version won't install over the old one, the two were signed
with different keys: uninstall Scribe first. This deletes its threads
and downloaded models.

## First run: speech models

Scribe needs two models from the
[sherpa-onnx releases](https://github.com/k2-fsa/sherpa-onnx/releases):
a streaming Zipformer for the live text and Parakeet TDT 0.6B v2 for the
corrected text. Tap **Download models** to fetch them, about 610 MB in
all, into the app's private storage. Wi-Fi is recommended. The download
continues if you switch apps, and if it fails, **Retry** resumes where it
stopped.

The models stay loaded while you use Scribe and are unloaded to free
memory after it has been out of sight for 10 minutes. Tap **Load
models** to load them again.

## Recording

Pick a source at the top:

* **Mic**: your voice.
* **Phone**: audio other apps are playing. Android asks for permission
  to capture the screen each time you start, since that is how apps
  reach other apps' audio. Stopping the capture from the system's
  notification stops the recording.
* **Both**: the mic and the phone audio as separate speakers, Me and
  Phone, so you can transcribe both sides of a call.

Then tap the record button. The level meter shows what the mic hears.
Faint text is the live guess, which is replaced by the corrected
paragraph when you pause.
While recording:

* **❚❚ / ▶** pauses and resumes.
* **¶** starts a new paragraph now instead of waiting for a pause.
* The transcript follows the newest text. Scroll up to read back, and
  **↓ Jump to latest** returns.

**Copy** and **Share** send the whole thread as text, and **Clear**
empties it. Scribe asks for the notification permission on first
recording so the Pause/Resume and Stop controls can appear there.

## Threads

Transcripts are kept as threads. **☰** opens the list, most recent
first, with **New thread** at the top. Tap a thread to open it, or
long-press it to rename or delete it. Tapping the title renames the open
thread. New threads are named with the date and time.

Recording adds to the open thread. While a recording or a file is being
transcribed, the other threads can't be opened.

## Playing back

With **Keep audio (.opus)** on in **⚙** → **Recording**, tap any word of a
transcript, once the recording has stopped, to hear it from that word.
The words light up as they are spoken and the transcript scrolls along.
While it plays, **« 10s** and **10s »** (back and ahead ten seconds),
**Pause**/**Resume** and **Stop** take the place of the source picker,
with the time below. Tapping the transcript also pauses: the
current word stays highlighted and you can scroll freely until you
resume, or tap another word to play from there. Nothing else is
shown while it plays; **Stop** brings back recording. Playback carries on
with the app in the background or the screen off, with the same controls
in the notification and on the lock screen (and headset buttons). It
pauses for a call or another app's audio, and when headphones are
unplugged. A Both recording plays the mic and the
phone audio together.

Playback runs through all of a thread's recordings in order, one after
another, and the time shown is for the whole thread. The green ▶ above
a recording's first paragraph plays from its start; long-press it to
delete that one recording (its audio and its text). With **Keep non-speech audio** on, a recording in which
no words were heard appears as "▶ No words detected" where it happened,
or, in a thread with no text at all, as a big ▶ with "No words detected
in log"; it plays from its first sound, though the recording is kept
whole. Long-pressing still selects text to copy, and the selection menu also
has **Edit**, to retype that paragraph, and **Cut**, which removes the
selected words and skips their audio on playback (the recording itself
is kept whole). Paragraphs recorded without
the setting have no audio.

## Transcribing a file

Share an audio or video file to Scribe from another app (**Transcribe
with Scribe**), or open one with it from a file manager. Scribe starts a
new thread named after the file and transcribes it faster than real
time, with progress in the app and the notification.

## Settings

**⚙** lists the settings pages and About.

**Microphone**: changes apply the next time the mic starts, so pause and
resume to apply them mid-recording.

* **Mic**: **Bluetooth headset when connected** (the default) records
  from a connected headset's mic, and the status line names the input in
  use. **Phone mic always** ignores headsets.
* **Mic boost**: **Auto (raise quiet speech)** brings quiet speech up to
  a steady level without raising background noise into words. **Off**,
  **+6 dB**, **+12 dB** and **+18 dB** apply a fixed boost instead.
  Phone audio and files are never boosted.

**Recording**:

* **Keep audio (.opus)**: saves each recording's audio (and a shared
  file's) with its transcript, along with when each word was said, so
  you can [play it back](#playing-back). Off by default. The audio is
  deleted with its thread, or by **Clear**.
* **Keep non-speech audio**: with Keep audio on, also keeps recordings
  in which no words were detected. Off by default, so those are deleted
  when they finish, as is a recording once all its text has been cut.

**Keyboard**: a shortcut to turn on the [voice typing
keyboard](#voice-typing-keyboard).

**Display**: the transcript's font size, five steps with the default in
the middle.

**About**: the app's version and a link to its source.

## Voice typing keyboard

Scribe voice typing dictates into any app's text field.

1. In **⚙** → **Keyboard**, tap **Keyboard settings** and turn on **Scribe voice
   typing**.
2. In any app, open the keyboard and pick Scribe with the keyboard
   switcher.
3. It starts listening right away and shows the transcript as you talk.
   **Insert** types the finished text and switches back to your previous
   keyboard. **Cancel** discards it, and **⌨** switches keyboards
   without inserting.

The keyboard uses the models the app downloaded, so open Scribe once
first. It can't listen while the app is recording.

## Building from source

Needs Rust with the `aarch64-linux-android` target,
[cargo-ndk](https://github.com/bbqsrc/cargo-ndk), the Android NDK, the
Android SDK, a JDK 17 or newer and [just](https://github.com/casey/just).
The recipes default to Homebrew's locations on macOS. Set
`ANDROID_NDK_HOME`, `ANDROID_HOME` and `JAVA_HOME` to use others.

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
just android-app          # debug APK, installed with adb if a device is connected
just android-apk          # release APK in dist/
```

`just android-apk` signs with the keystore in `SCRIBE_KEYSTORE` (plus
`SCRIBE_KEYSTORE_PASSWORD`, `SCRIBE_KEY_ALIAS` and `SCRIBE_KEY_PASSWORD`)
when it is set, and with the debug key otherwise. The release workflow
builds it on every version tag, using a keystore from the repository's
secrets when there is one (`SCRIBE_KEYSTORE_BASE64` and the three
passwords). `just android-keystore` creates that keystore in
`~/scribe.jks` and stores the secrets with `gh`. Each release must be
signed with the same key to install over the last, so if the secrets are
ever lost along with the keystore, the next release needs a fresh install.
`just android-app` signs with that same keystore when `~/scribe.jks`
exists, so local builds and releases install over each other. Its
password is `SCRIBE_KEYSTORE_PASSWORD` if set (for example in `.env`),
else `android`; you're asked only when that doesn't open it.

The app is in [android/](android/), Kotlin and Jetpack Compose over a
JNI bridge, [crates/vox_android](crates/vox_android), to the same
transcription engine as the desktop scribe.
