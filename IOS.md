# Scribe on iOS (work in progress)

A minimal SwiftUI dictation app in `ios/` over `crates/vox_ios`, a
staticlib with a small C ABI (`ios/Scribe/vox_ios.h`) around
vox_transcribe's engine: Parakeet TDT 0.6b v2 (int8) for passes 2 and 3,
the streaming Zipformer for live partials, 4 threads each, as on Android.
The mic is AVAudioEngine, converted to 16 kHz mono in Swift. iOS 17+,
arm64 devices.

sherpa-onnx only ships iOS as a shared xcframework (SherpaOnnxC, with
onnxruntime inside), so vox_ios turns on `sherpa-shared` for iOS targets
only, builds into `target/ios`, and the app embeds the xcframework.

## Build and install

Needs Xcode (xcode-select may stay on the Command Line Tools) signed in to an Apple ID (Settings > Accounts; a free
Personal Team works), `brew install xcodegen`, `rustup target add
aarch64-apple-ios`, and the device paired once over USB (Trust) with
Developer Mode on; after that it installs over Wi-Fi.

```
just ios-app        # build, sign, install and launch
just ios-models     # copy the models from this Mac into the app
```

On the first install with a free team, trust the certificate on the
device: Settings > General > VPN & Device Management. Apps signed by a
free team expire after 7 days; `just ios-app` again re-signs.

Models live in the app's Documents/models (`parakeet-tdt-0.6b-v2/`,
`streaming-zipformer/`), which Finder's file sharing and the Files app
can also reach. There's no in-app downloader yet.

## Benchmark

Launched with `--bench`, the app transcribes Documents/bench.wav (16 kHz
mono) as fast as it can, with and without pass 1, and logs the speed and
peak memory to the console:

```
ffmpeg -i talk.opus -ac 1 -ar 16000 -c:a pcm_f32le bench.wav
xcrun devicectl device copy to --device <udid> --domain-type appDataContainer \
  --domain-identifier com.enigmacurry.rpg-vox-scribe --source bench.wav --destination Documents/bench.wav
xcrun devicectl device process launch --device <udid> --terminate-existing --console \
  com.enigmacurry.rpg-vox-scribe --bench
```

iPad Pro 2nd gen (A10X, 4 GB, iPadOS 17), 106 s of speech: models load in
3.6 s (714 MB); passes 2+3 alone run 3.4x realtime, with pass 1 too 2.6x;
peak footprint about 890 MB.
