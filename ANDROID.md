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
* [Transcribing a file](#transcribing-a-file)
* [Microphone settings](#microphone-settings)
* [Voice typing keyboard](#voice-typing-keyboard)
* [Building from source](#building-from-source)

## Install

Download `scribe-<version>-android-arm64.apk` from the
[releases page](https://github.com/EnigmaCurry/rpg_vox/releases) on the
phone and open it. Android asks you to allow installs from your browser
or file manager the first time.

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

## Transcribing a file

Share an audio or video file to Scribe from another app (**Transcribe
with Scribe**), or open one with it from a file manager. Scribe starts a
new thread named after the file and transcribes it faster than real
time, with progress in the app and the notification.

## Microphone settings

**⚙** opens the microphone settings. Changes apply the next time the mic
starts, so pause and resume to apply them mid-recording.

* **Mic**: **Bluetooth headset when connected** (the default) records
  from a connected headset's mic, and the status line names the input in
  use. **Phone mic always** ignores headsets.
* **Mic boost**: **Auto (raise quiet speech)** brings quiet speech up to
  a steady level without raising background noise into words. **Off**,
  **+6 dB**, **+12 dB** and **+18 dB** apply a fixed boost instead.
  Phone audio and files are never boosted.

## Voice typing keyboard

Scribe voice typing dictates into any app's text field.

1. In **⚙**, tap **Keyboard settings** and turn on **Scribe voice
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
`~/scribe.jks` and stores the secrets with `gh`. Back the keystore up:
each release must be signed with the same key to install over the last.

The app is in [android/](android/), Kotlin and Jetpack Compose over a
JNI bridge, [crates/vox_android](crates/vox_android), to the same
transcription engine as the desktop scribe.
