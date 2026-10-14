import AVFoundation
import Foundation

/// Decodes the audio of `url` (anything AVFoundation reads: m4a, mp3,
/// wav, aiff, flac, caf, video files…) to mono at `rate` Hz, handing it to
/// `sink` in blocks. `durationMs` is called once up front (0 when
/// unknown). Stops early when `cancelled` says so.
func decodeAudio(
    _ url: URL,
    rate: Int,
    durationMs: (Int64) -> Void,
    cancelled: () -> Bool,
    sink: (UnsafeMutablePointer<Float>, Int) -> Void
) throws {
    let asset = AVURLAsset(url: url)
    let wait = DispatchSemaphore(value: 0)
    var loaded: Result<(AVAssetTrack, CMTime), Error>?
    Task {
        do {
            guard let track = try await asset.loadTracks(withMediaType: .audio).first else {
                throw NativeError(message: "no audio track")
            }
            loaded = .success((track, try await asset.load(.duration)))
        } catch {
            loaded = .failure(error)
        }
        wait.signal()
    }
    wait.wait()
    let (track, duration) = try loaded!.get()
    durationMs(duration.isNumeric ? Int64(duration.seconds * 1000) : 0)

    let reader = try AVAssetReader(asset: asset)
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
        AVFormatIDKey: kAudioFormatLinearPCM,
        AVLinearPCMIsFloatKey: true,
        AVLinearPCMBitDepthKey: 32,
        AVLinearPCMIsNonInterleaved: false,
        AVLinearPCMIsBigEndianKey: false,
        AVSampleRateKey: rate,
        AVNumberOfChannelsKey: 1,
    ])
    output.alwaysCopiesSampleData = false
    reader.add(output)
    guard reader.startReading() else {
        throw reader.error ?? NativeError(message: "can't read the audio")
    }
    defer { if reader.status == .reading { reader.cancelReading() } }
    var buf = [Float]()
    while !cancelled(), let sample = output.copyNextSampleBuffer() {
        guard let block = CMSampleBufferGetDataBuffer(sample) else { continue }
        let bytes = CMBlockBufferGetDataLength(block)
        let n = bytes / MemoryLayout<Float>.size
        if n == 0 { continue }
        if buf.count < n { buf = [Float](repeating: 0, count: n) }
        buf.withUnsafeMutableBytes { raw in
            _ = CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: bytes, destination: raw.baseAddress!)
        }
        buf.withUnsafeMutableBufferPointer { sink($0.baseAddress!, n) }
    }
    if reader.status == .failed {
        throw reader.error ?? NativeError(message: "decoding failed")
    }
}
