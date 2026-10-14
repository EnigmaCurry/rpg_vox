import AVFoundation
import Foundation
import MediaPlayer
import Observation

/// Opus decodes at 48 kHz.
private let PLAY_RATE = 48_000

/// What is playing: the files on the timeline, each with its start (ms).
struct Playing: Equatable {
    let offsets: [String: Int64]
}

/// Plays a thread's kept recordings from a point, their sources mixed,
/// through an AVAudioPlayerNode, as the Android app's `Playback` does
/// through an AudioTrack: a worker decodes 50 ms blocks with Rust and
/// keeps about 200 ms queued. `positionMs` is where playback is on the
/// timeline, for the karaoke highlight. With lock-screen and headset
/// controls; pauses for calls and when headphones go.
@MainActor @Observable
final class Playback {
    static let shared = Playback()

    private(set) var playing: Playing?
    /// Playing, but held where it is.
    private(set) var paused = false
    /// Length of the timeline (ms), once known.
    private(set) var lengthMs: Int64 = 0

    @ObservationIgnored private var worker: Worker?
    @ObservationIgnored private var fromMs: Int64 = 0
    @ObservationIgnored private var dir = URL(fileURLWithPath: "/")
    @ObservationIgnored private var files: [AudioFile] = []
    @ObservationIgnored private var skips: [ClosedRange<Int64>] = []
    @ObservationIgnored private var title = ""
    @ObservationIgnored private var retries = 0

    private init() {
        let nc = NotificationCenter.default
        nc.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { note in
            guard let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  AVAudioSession.InterruptionType(rawValue: raw) == .began else { return }
            MainActor.assumeIsolated { Playback.shared.pause() }
        }
        nc.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main) { note in
            guard let raw = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                  AVAudioSession.RouteChangeReason(rawValue: raw) == .oldDeviceUnavailable else { return }
            // Headphones unplugged: don't carry on out of the speaker.
            MainActor.assumeIsolated { Playback.shared.pause() }
        }
        let c = MPRemoteCommandCenter.shared()
        c.playCommand.addTarget { _ in MainActor.assumeIsolated { Playback.shared.resume() }; return .success }
        c.pauseCommand.addTarget { _ in MainActor.assumeIsolated { Playback.shared.pause() }; return .success }
        c.togglePlayPauseCommand.addTarget { _ in
            MainActor.assumeIsolated {
                let p = Playback.shared
                if p.paused { p.resume() } else { p.pause() }
            }
            return .success
        }
        c.stopCommand.addTarget { _ in MainActor.assumeIsolated { Playback.shared.stop() }; return .success }
        c.skipForwardCommand.preferredIntervals = [10]
        c.skipBackwardCommand.preferredIntervals = [10]
        c.skipForwardCommand.addTarget { _ in MainActor.assumeIsolated { Playback.shared.skip(10_000) }; return .success }
        c.skipBackwardCommand.addTarget { _ in MainActor.assumeIsolated { Playback.shared.skip(-10_000) }; return .success }
        c.changePlaybackPositionCommand.addTarget { e in
            guard let e = e as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            MainActor.assumeIsolated { Playback.shared.seekTo(Int64(e.positionTime * 1000)) }
            return .success
        }
        setCommands(enabled: false)
    }

    private func setCommands(enabled: Bool) {
        let c = MPRemoteCommandCenter.shared()
        for cmd in [c.playCommand, c.pauseCommand, c.togglePlayPauseCommand, c.stopCommand,
                    c.skipForwardCommand, c.skipBackwardCommand, c.changePlaybackPositionCommand] {
            cmd.isEnabled = enabled
        }
    }

    /// Position on the timeline (ms).
    func positionMs() -> Int64 { worker?.positionMs() ?? fromMs }

    /// Play `files` (in `dir`), placed on one timeline, from `at` on it,
    /// jumping over `skips`.
    func play(dir: URL, files: [AudioFile], at: Int64, skips: [ClosedRange<Int64>], title: String) {
        stop()
        if files.isEmpty { return }
        self.dir = dir
        self.files = files
        self.skips = skips.sorted { $0.lowerBound < $1.lowerBound }
        self.title = title
        retries = 0
        paused = false
        lengthMs = 0
        playing = Playing(offsets: Dictionary(files.map { ($0.file, $0.offsetMs) }, uniquingKeysWith: { a, _ in a }))
        do {
            try AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio)
            try AVAudioSession.sharedInstance().setActive(true)
        } catch {
            print("playback session: \(error)")
        }
        setCommands(enabled: true)
        startWorker(max(at, 0))
    }

    /// Pausing lets go of the output and resuming opens a new one at the
    /// same spot.
    func pause() {
        guard playing != nil, !paused else { return }
        let at = positionMs()
        stopWorker()
        fromMs = at
        paused = true
        updateNowPlaying()
    }

    func resume() {
        guard playing != nil, paused else { return }
        paused = false
        startWorker(fromMs)
    }

    /// Jump `deltaMs` back or ahead, staying paused if paused.
    func skip(_ deltaMs: Int64) { seekTo(positionMs() + deltaMs) }

    /// Jump to `ms` on the timeline, staying paused if paused.
    func seekTo(_ ms: Int64) {
        guard playing != nil else { return }
        let end = lengthMs > 0 ? lengthMs : .max
        let at = min(max(ms, 0), end)
        if paused {
            fromMs = at
            updateNowPlaying()
        } else {
            stopWorker()
            startWorker(at)
        }
    }

    func stop() {
        stopWorker()
        if playing != nil {
            playing = nil
            paused = false
            setCommands(enabled: false)
            MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
    }

    private func startWorker(_ from: Int64) {
        guard playing != nil else { return }
        fromMs = from
        let w = Worker(dir: dir, files: files, skips: skips, from: from)
        worker = w
        w.onLength = { len in
            Task { @MainActor in
                if len > self.lengthMs {
                    self.lengthMs = len
                    self.updateNowPlaying()
                }
            }
        }
        w.onEnd = { at, died in
            Task { @MainActor in
                // Ended by itself: clear the state, unless a newer playback took over.
                guard self.worker === w else { return }
                self.worker = nil
                if died && self.retries < 3 {
                    self.retries += 1
                    print("audio output died at \(at) ms; reopening")
                    self.startWorker(at)
                } else {
                    self.fromMs = at
                    self.stop()
                }
            }
        }
        w.start()
        updateNowPlaying()
    }

    private func stopWorker() {
        guard let w = worker else { return }
        worker = nil
        w.stop()
    }

    private func updateNowPlaying() {
        guard playing != nil else { return }
        var info: [String: Any] = [
            MPMediaItemPropertyTitle: title.isEmpty ? "Scribe" : title,
            MPMediaItemPropertyArtist: "Scribe",
            MPNowPlayingInfoPropertyElapsedPlaybackTime: Double(positionMs()) / 1000,
            MPNowPlayingInfoPropertyPlaybackRate: paused ? 0.0 : 1.0,
        ]
        if lengthMs > 0 { info[MPMediaItemPropertyPlaybackDuration] = Double(lengthMs) / 1000 }
        MPNowPlayingInfoCenter.default().nowPlayingInfo = info
        MPNowPlayingInfoCenter.default().playbackState = paused ? .paused : .playing
    }
}

/// One playback run, from a point until stopped or the end, on its own
/// thread with its own engine.
private final class Worker: @unchecked Sendable {
    let dir: URL
    let files: [AudioFile]
    let skips: [ClosedRange<Int64>]
    let from: Int64
    var onLength: (Int64) -> Void = { _ in }
    /// Where it got to, and whether the output died under it.
    var onEnd: (Int64, Bool) -> Void = { _, _ in }

    private let run = Flag()
    private let finished = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var node: AVAudioPlayerNode?
    private var startMs: Int64
    /// Where playback jumped over a cut, as (frames queued before it,
    /// timeline ms after it), so `positionMs` follows the jumps.
    private var jumps: [(Int64, Int64)] = []

    init(dir: URL, files: [AudioFile], skips: [ClosedRange<Int64>], from: Int64) {
        self.dir = dir
        self.files = files
        self.skips = skips
        self.from = from
        startMs = from
    }

    func start() {
        let t = Thread { self.loop() }
        t.name = "vox-play"
        t.qualityOfService = .userInitiated
        t.start()
    }

    /// Stops and waits for the thread to be done.
    func stop() {
        run.set()
        lock.withLock { node }?.stop()
        finished.wait()
    }

    /// Frames the node has played since it started.
    private func played() -> Int64 {
        guard let node = lock.withLock({ node }), let t = node.lastRenderTime, t.isSampleTimeValid,
              let p = node.playerTime(forNodeTime: t) else { return 0 }
        return max(p.sampleTime, 0)
    }

    func positionMs() -> Int64 {
        let frames = played()
        let (at, ms) = lock.withLock { jumps.last { $0.0 <= frames } ?? (0, startMs) }
        return ms + (frames - at) * 1000 / Int64(PLAY_RATE)
    }

    /// One file on the timeline, from `startMs` to `endMs`; once placed,
    /// `silence` to play before its audio, then at most `left` samples.
    private final class Source {
        let handle: Handle
        let startMs: Int64
        let endMs: Int64
        var silence: Int64 = 0
        var left: Int64 = 0
        var done = false

        init(handle: Handle, startMs: Int64, endMs: Int64) {
            self.handle = handle
            self.startMs = startMs
            self.endMs = endMs
        }

        /// Ready to play from `at` on the timeline.
        func place(_ at: Int64) {
            let begin = max(at, startMs)
            done = endMs <= begin
            silence = done ? 0 : (begin - at) * Int64(PLAY_RATE) / 1000
            left = done ? 0 : (endMs - begin) * Int64(PLAY_RATE) / 1000
            if !done { Native.playerSeek(handle, begin - startMs) }
        }
    }

    private func loop() {
        var sources: [Source] = []
        var died = false
        let engine = AVAudioEngine()
        let player = AVAudioPlayerNode()
        let format = AVAudioFormat(standardFormatWithSampleRate: Double(PLAY_RATE), channels: 1)!
        defer {
            let at = positionMs()
            lock.withLock { node = nil }
            player.stop()
            engine.stop()
            sources.forEach { Native.playerFree($0.handle) }
            onEnd(at, died)
            finished.signal()
        }
        for f in files {
            guard let h = try? Native.playerOpen(dir.appendingPathComponent(f.file).path) else {
                print("can't open \(f.file)")
                continue
            }
            let end = min(f.untilMs, f.offsetMs + Native.playerLength(h))
            onLength(end)
            sources.append(Source(handle: h, startMs: f.offsetMs, endMs: end))
        }
        if sources.isEmpty { return }
        // Starting inside a cut starts after it.
        var pos = skips.first { $0.contains(from) }?.upperBound ?? from
        lock.withLock { startMs = pos }
        sources.forEach { $0.place(pos) }

        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: format)
        do {
            try engine.start()
        } catch {
            print("playback: \(error)")
            died = true
            return
        }
        player.play()
        lock.withLock { node = player }

        let block = PLAY_RATE / 20 // 50 ms
        let queued = DispatchSemaphore(value: 4) // ~200 ms ahead
        var got = [Float](repeating: 0, count: block)
        var written: Int64 = 0
        // Timeline position of the next sample, in samples (exact).
        var posSamples = pos * Int64(PLAY_RATE) / 1000
        while !run.value {
            pos = posSamples * 1000 / Int64(PLAY_RATE)
            // At a cut: jump past it.
            if let cut = skips.first(where: { $0.contains(pos) && $0.upperBound > pos }) {
                sources.forEach { $0.place(cut.upperBound) }
                posSamples = cut.upperBound * Int64(PLAY_RATE) / 1000
                let w = written
                lock.withLock { jumps.append((w, cut.upperBound)) }
                continue
            }
            // Stop short of the next cut.
            var room = block
            if let next = skips.first(where: { $0.lowerBound > pos })?.lowerBound {
                room = Int(min(Int64(block), max(1, next * Int64(PLAY_RATE) / 1000 - posSamples)))
            }
            guard let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(room)) else { break }
            let mix = buf.floatChannelData![0]
            mix.initialize(repeating: 0, count: room)
            var produced = 0
            for s in sources { produced = max(produced, add(s, mix, &got, room)) }
            if produced == 0 { break }
            posSamples += Int64(produced)
            for i in 0..<produced { mix[i] = min(max(mix[i], -1), 1) }
            buf.frameLength = AVAudioFrameCount(produced)
            queued.wait()
            if run.value { break }
            player.scheduleBuffer(buf) { queued.signal() }
            written += Int64(produced)
            if !engine.isRunning {
                // The output went away (e.g. a route change); pick up
                // where it got to on a new one.
                died = true
                break
            }
        }
        // Let the queued tail play out.
        while !run.value && !died && played() < written {
            if !engine.isRunning {
                died = true
                break
            }
            usleep(20_000)
        }
    }

    /// Mix up to `room` of `s`'s next samples into `mix`; how many it covered (0 once it's over).
    private func add(_ s: Source, _ mix: UnsafeMutablePointer<Float>, _ got: inout [Float], _ room: Int) -> Int {
        var i = 0
        if s.silence > 0 {
            let k = Int(min(s.silence, Int64(room)))
            s.silence -= Int64(k)
            i = k
        }
        if s.done || i == room { return i }
        let want = Int(min(Int64(room - i), s.left))
        if want == 0 {
            s.done = true
            return i
        }
        if got.count < want { got = [Float](repeating: 0, count: want) }
        let n = got.withUnsafeMutableBufferPointer { Native.playerRead(s.handle, $0.baseAddress!, want) }
        s.left -= Int64(n)
        if n < want || s.left == 0 { s.done = true }
        for j in 0..<n { mix[i + j] += got[j] }
        return i + n
    }
}
