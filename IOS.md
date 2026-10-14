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

Needs Xcode signed in to an Apple ID (Settings > Accounts; a free
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
