# Scribe for iOS (work in progress)

The [Android app](ANDROID.md)'s features on iPhone and iPad, iOS 17 or
newer, arm64: live transcription with the same models and passes, all on
the device.

* **Transcriptions** (🎙️): record from the mic, live text faint until
  the corrected paragraph replaces it. Pause and resume, Jump to latest,
  Copy, Share, Clear. Recording carries on with the screen locked.
* **Dictations** (✍️): write by voice and keep editing anywhere. Tap the
  text to move the purple cursor where new speech goes; ¶ starts a
  paragraph, ⌫ deletes the sentence before the cursor (or the
  selection), the keyboard button edits by hand, and selected words can
  be deleted from the selection menu.
* **One list** of both, most recently changed first, with New, Rename
  and Delete (long-press, or the ⋯ menu).
* **Keep audio (.opus)** (⋯ › Recording): tap any word to hear it from
  there, the words lit as they're heard; back and ahead 10 s, pause,
  stop, also on the lock screen and headset. ▶ above a recording plays
  it from the start (long-press: delete that clip). Keep non-speech
  audio, as on Android.
* **Edit and Cut** in the text selection menu: retype a paragraph, or
  remove words and skip their audio.
* **Files**: ⋯ › Transcribe a file…, or Open in Scribe from the share
  sheet or Files, for audio and video (whatever AVFoundation reads).
* **Microphone** settings: a Bluetooth headset's mic or always the
  built-in one, and mic boost (Auto, Off, +6/+12/+18 dB). **Display**:
  font size. **About**: version and changelog.
* **Models** are downloaded in the app (about 610 MB, resumable, and it
  carries on in the background).

Not on iOS: transcribing other apps' audio (Phone and Both on Android;
iOS only offers that to screen-broadcast extensions, too small to run
the models), the voice typing keyboard (keyboards can't use the mic),
and Pause/Stop in a notification while recording.

The threads live in `scribe.db` with the Android app's schema, so
they're the desktop chat database's too.

## How it's built

`ios/` is a SwiftUI app (an XcodeGen spec, `ios/project.yml`) over
`crates/vox_ios`, a staticlib with a C ABI (`ios/Scribe/vox_ios.h`) that
mirrors crates/vox_android's JNI: the engine, Opus recording and
playback, the model unpacker, word re-timing. Swift does the audio
(AVAudioEngine) and the UI, ported from the Kotlin app file by file.

sherpa-onnx only ships iOS as a shared xcframework (SherpaOnnxC, with
onnxruntime inside), so vox_ios turns on `sherpa-shared` for iOS targets
only and builds into `target/ios`, and the app embeds the xcframework.

## Build and install

Needs Xcode signed in to an Apple ID (Settings › Accounts; a free
Personal Team works, with an Apple Development certificate made under
Manage Certificates), `brew install xcodegen`, `rustup target add
aarch64-apple-ios aarch64-apple-ios-sim`, and the device paired once
over USB (Trust, then `xcrun devicectl manage pair --device <udid>`)
with Developer Mode on.

```
just ios-app        # build, sign, install and launch on the device
just ios-models     # copy this Mac's models into it instead of downloading
just ios-sim        # build and run in the Simulator, with this Mac's models
```

Apps signed by a free team expire after 7 days; `just ios-app` again
re-signs, keeping the data.

For testing without touch or a mic (the Simulator): launch with
`-mic_file NAME` and Documents/NAME stands in for the mic, in real time;
`-transcribe_at_launch NAME` transcribes it as a file, and with
`-play_at_launch YES` then plays it.

```
xcrun simctl launch booted com.enigmacurry.rpg-vox-scribe -mic_file talk.m4a
```

## Speed

iPad Pro 2nd gen (A10X, 4 GB, iPadOS 17): the models load in 3.4 s
(about 715 MB); live, the corrected text lands about 2.3 s after a
pause, and a recording is finished 1.3 s after Stop, at about 840 MB.
