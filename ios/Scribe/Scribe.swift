import AVFoundation
import Foundation

/// Sample rate the engine runs at, as on Android.
private let rate: Double = 16_000
/// ONNX threads for each model, as on Android.
private let threads: Int32 = 4

struct Clip: Decodable {
    let id: String
    let start_ms: UInt64
    let duration_ms: UInt64?
    let text: String
    let stage: String

    var partial: Bool { stage == "partial" }
}

struct Paragraph: Decodable, Identifiable {
    let id: String
    let start_ms: UInt64
    let text: String
    let clips: [Clip]
    let hardened: Bool
}

private struct RawEvent: Decodable {
    let t: String
    let change: String?
    let p: Paragraph?
    let id: String?
    let rms: Float?
    let speaking: Bool?
}

private struct Transcript: Decodable {
    let paragraphs: [Paragraph]
}

/// The models, one dictation at a time and its transcript, for the UI.
@MainActor
final class Scribe: ObservableObject {
    enum Phase: Equatable {
        case loading
        case noModels(String)
        case failed(String)
        case idle
        case recording
        case finishing
    }

    @Published var phase: Phase = .loading
    @Published var paragraphs: [Paragraph] = []
    @Published var rms: Float = 0
    @Published var speaking = false
    /// Speed and memory, shown under the controls.
    @Published var stats = ""

    private var models: OpaquePointer?
    private var session: OpaquePointer?
    private let engine = AVAudioEngine()
    private var loadSeconds = 0.0
    /// Samples pushed this dictation; the poller turns them into lag.
    private let pushed = Counter()
    /// How far behind the audio pass 2 finished its last clip.
    private var lastLagMs: Int?
    /// Set once the dictation is finished; the poller then exits.
    private var stopPolling = Flag()

    static var modelsDir: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("models")
    }

    var text: String {
        paragraphs.map(\.text).filter { !$0.isEmpty }.joined(separator: "\n\n")
    }

    func load() {
        phase = .loading
        let dir = Self.modelsDir
        for name in ["parakeet-tdt-0.6b-v2", "streaming-zipformer"] {
            if !FileManager.default.fileExists(atPath: dir.appendingPathComponent(name).path) {
                phase = .noModels("Copy \(name) into \(dir.path) (Finder file sharing or `just ios-models`).")
                return
            }
        }
        Task.detached(priority: .userInitiated) {
            let t0 = Date()
            var err: UnsafeMutablePointer<CChar>?
            let m = vox_load(dir.path, threads, &err)
            let message = err.map { String(cString: $0) }
            vox_string_free(err)
            let secs = Date().timeIntervalSince(t0)
            await MainActor.run {
                if let m {
                    self.models = m
                    self.loadSeconds = secs
                    self.phase = .idle
                    self.updateStats()
                } else {
                    self.phase = .failed(message ?? "loading the models failed")
                }
            }
        }
    }

    func start() {
        guard let models, session == nil else { return }
        do {
            let audio = AVAudioSession.sharedInstance()
            try audio.setCategory(.record, mode: .measurement)
            try audio.setActive(true)
        } catch {
            phase = .failed("audio session: \(error.localizedDescription)")
            return
        }
        AVAudioApplication.requestRecordPermission { granted in
            Task { @MainActor in
                if granted {
                    self.begin(models)
                } else {
                    self.phase = .failed("Microphone access is off: Settings > Privacy & Security > Microphone > Scribe.")
                }
            }
        }
    }

    private func begin(_ models: OpaquePointer) {
        paragraphs = []
        lastLagMs = nil
        pushed.reset()
        stopPolling = Flag()
        let s = vox_start(models, UInt32(rate), true)!
        session = s

        let input = engine.inputNode
        let from = input.outputFormat(forBus: 0)
        let to = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false)!
        guard let converter = AVAudioConverter(from: from, to: to) else {
            phase = .failed("can't convert \(from) to 16 kHz mono")
            return
        }
        input.installTap(onBus: 0, bufferSize: 4096, format: from, block: Self.tap(s, converter, pushed))
        do {
            engine.prepare()
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            phase = .failed("microphone: \(error.localizedDescription)")
            return
        }
        phase = .recording
        poll(s)
    }

    /// Converts each mic buffer to 16 kHz mono and pushes it. Runs on the
    /// audio engine's thread, so it's built outside the main actor.
    nonisolated private static func tap(
        _ s: OpaquePointer, _ converter: AVAudioConverter, _ pushed: Counter
    ) -> AVAudioNodeTapBlock {
        let to = converter.outputFormat
        let ratio = to.sampleRate / converter.inputFormat.sampleRate
        return { buf, _ in
            let cap = AVAudioFrameCount(Double(buf.frameLength) * ratio) + 16
            guard let out = AVAudioPCMBuffer(pcmFormat: to, frameCapacity: cap) else { return }
            var fed = false
            var error: NSError?
            converter.convert(to: out, error: &error) { _, status in
                if fed {
                    status.pointee = .noDataNow
                    return nil
                }
                fed = true
                status.pointee = .haveData
                return buf
            }
            guard error == nil, let ch = out.floatChannelData, out.frameLength > 0 else { return }
            vox_push(s, ch[0], Int(out.frameLength))
            pushed.add(Int(out.frameLength))
        }
    }

    /// Feeds engine events to the UI until `stopPolling` is set.
    private func poll(_ s: OpaquePointer) {
        let pushed = pushed
        let stopped = stopPolling
        Thread.detachNewThread { [weak self] in
            while !stopped.value {
                guard let raw = vox_poll(s, 200) else { continue }
                let json = Data(String(cString: raw).utf8)
                vox_string_free(raw)
                guard let events = try? JSONDecoder().decode([RawEvent].self, from: json), !events.isEmpty else {
                    continue
                }
                let nowMs = pushed.value * 1000 / Int(rate)
                Task { @MainActor in self?.apply(events, nowMs: nowMs) }
            }
            stopped.done.signal()
        }
    }

    private func apply(_ events: [RawEvent], nowMs: Int) {
        for ev in events {
            switch ev.t {
            case "paragraph":
                guard let p = ev.p else { continue }
                if ev.change == "final", let c = p.clips.last(where: { !$0.partial }), let d = c.duration_ms {
                    lastLagMs = nowMs - Int(c.start_ms + d)
                }
                if let i = paragraphs.firstIndex(where: { $0.id == p.id }) {
                    paragraphs[i] = p
                } else {
                    let i = paragraphs.firstIndex(where: { $0.start_ms > p.start_ms }) ?? paragraphs.endIndex
                    paragraphs.insert(p, at: i)
                }
            case "removed":
                paragraphs.removeAll { $0.id == ev.id }
            case "level":
                rms = ev.rms ?? 0
                speaking = ev.speaking ?? false
            default:
                break
            }
        }
        updateStats()
    }

    func newParagraph() {
        if let session { vox_break_paragraph(session) }
    }

    func stop() {
        guard let s = session, phase == .recording else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        phase = .finishing
        let stopped = stopPolling
        Task.detached(priority: .userInitiated) {
            let t0 = Date()
            let raw = vox_finish(s)
            let secs = Date().timeIntervalSince(t0)
            let transcript = raw.flatMap { try? JSONDecoder().decode(Transcript.self, from: Data(String(cString: $0).utf8)) }
            vox_string_free(raw)
            await MainActor.run {
                if let transcript { self.paragraphs = transcript.paragraphs }
                self.session = nil
                self.phase = .idle
                self.updateStats(finish: secs)
            }
            // The poller may be inside vox_poll; free once it's out.
            stopped.set()
            DispatchQueue.global().async {
                stopped.done.wait()
                vox_free(s)
            }
        }
    }

    private func updateStats(finish: Double? = nil) {
        var parts = [String(format: "models %.1f s", loadSeconds)]
        if let lastLagMs { parts.append("pass-2 lag \(lastLagMs) ms") }
        if let finish { parts.append(String(format: "finish %.1f s", finish)) }
        parts.append("memory \(footprintMB()) MB")
        stats = parts.joined(separator: " · ")
    }
}

/// A one-way flag, with a semaphore the poller signals on its way out.
final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var on = false
    let done = DispatchSemaphore(value: 0)
    var value: Bool { lock.withLock { on } }
    func set() { lock.withLock { on = true } }
}

/// Samples pushed, shared with the audio tap and the poller.
final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var n = 0
    var value: Int { lock.withLock { n } }
    func add(_ k: Int) { lock.withLock { n += k } }
    func reset() { lock.withLock { n = 0 } }
}

/// The app's physical footprint, what iOS's memory limit counts.
func footprintMB() -> Int {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
    let kr = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    return kr == KERN_SUCCESS ? Int(info.phys_footprint / 1_048_576) : -1
}
