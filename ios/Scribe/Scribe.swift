import AVFoundation
import Foundation
import Observation
import UIKit

/// Threads per recognizer (Parakeet and Zipformer each), as on Android.
private let THREADS: Int32 = 4
/// Without "Keep non-speech audio", longer quiet between words plays as this long.
private let MAX_GAP_MS: Int64 = 1_000
/// Louder than this (RMS dBFS) is sound, where a recording with no words starts playing.
private let SOUND_DB: Float = -45
/// Started this far before that sound.
private let SOUND_PAD_MS: Int64 = 300
/// How far file decoding may run ahead of pass 2, bounding queued audio.
private let FILE_AHEAD_MS: Int64 = 120_000

struct ClipUi: Equatable {
    let text: String
    let partial: Bool
}

/// One paragraph. `source` is its speaker when a recording has two
/// sources (none on iOS yet). With kept audio, `audio` is the file it was
/// heard in and `words` its words' times there.
struct Para: Identifiable, Equatable {
    let id: String
    var clips: [ClipUi]
    var source: String? = nil
    var audio: String? = nil
    var words: [Word] = []
    /// When it was said (epoch ms); the transcript is in this order.
    var atMs: Int64 = 0

    var text: String { clips.filter { !$0.text.trimmingCharacters(in: .whitespaces).isEmpty }.map(\.text).joined(separator: " ") }
    /// `text`'s words as shown.
    var shown: [String] { text.split(separator: " ").map(String.init) }
    var partial: Bool { clips.contains { $0.partial } }
    /// Has kept audio and word times, and is finished text.
    var playable: Bool { audio != nil && !words.isEmpty && !partial }

    /// Timing for shown word `i` of `shown`: one to one when the counts agree, else spread.
    func wordAt(_ i: Int, _ shown: Int) -> Word {
        if words.count == shown { return words[i] }
        return words[min(max(i * words.count / max(shown, 1), 0), words.count - 1)]
    }
}

enum Phase: Equatable {
    /// Models missing; nothing started yet, or the last attempt failed.
    case needModels(error: String?, resume: Bool)
    case installing(Step, Double)
    /// Installed but not in memory (not loaded yet, or released).
    case unloaded
    case loading
    case ready
    case failed(String)
}

/// A file being transcribed. `progress` is the share of its audio through pass 2.
struct FileJob: Equatable {
    let name: String
    var progress: Double
    /// Audio through pass 2 and the file's length (0 until known), in ms.
    var doneMs: Int64 = 0
    var totalMs: Int64 = 0
    let startedAt = Date()

    /// "1:23 / 10:05 · about 2:10 left": how far it's got, and the time
    /// left at the pace so far (once there's enough to go on).
    func detail() -> String? {
        guard totalMs > 0 else { return nil }
        let spent = Int64(Date().timeIntervalSince(startedAt) * 1000)
        var left = ""
        if doneMs >= 5_000 && spent > 0 {
            left = " · about \(clock(max(totalMs - doneMs, 0) * spent / doneMs)) left"
        }
        return "\(clock(doneMs)) / \(clock(totalMs))\(left)"
    }
}

/// m:ss, or h:mm:ss from an hour.
func clock(_ ms: Int64) -> String {
    let s = max(ms, 0) / 1000
    return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, s / 60 % 60, s % 60) : String(format: "%d:%02d", s / 60, s % 60)
}

/// Epoch ms now.
func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

/// A kept file placed on a timeline: it starts at `offsetMs` (its time
/// zero) and plays until `untilMs`.
struct AudioFile: Equatable {
    let file: String
    let offsetMs: Int64
    var untilMs: Int64 = .max
}

/// App-wide transcription state: the loaded models, the running sources,
/// their audio, and the open thread's transcript; the iOS twin of the
/// Android app's `Scribe` object. Main thread only, except where noted.
@MainActor @Observable
final class Scribe {
    static let shared = Scribe()

    @ObservationIgnored let models = ModelStore()
    @ObservationIgnored let threads: ThreadStore
    @ObservationIgnored private let defaults = UserDefaults.standard

    var phase: Phase = .unloaded
    /// Mic gain and which mic; applied when the mic (re)starts.
    var boost: Boost { didSet { defaults.set(boost.rawValue, forKey: "boost") } }
    var route: MicRoute { didSet { defaults.set(route.rawValue, forKey: "route") } }
    /// Save each recording's audio (.opus) with its transcript, for playback.
    var keepAudio: Bool { didSet { defaults.set(keepAudio, forKey: "keep_audio") } }
    /// With `keepAudio`, also keep recordings in which no words were detected.
    var keepNonSpeech: Bool { didSet { defaults.set(keepNonSpeech, forKey: "keep_non_speech") } }
    /// Start recording (or dictating) as soon as a new one is made with New.
    var autoStart: Bool { didSet { defaults.set(autoStart, forKey: "auto_start") } }
    /// Just made with New and `autoStart` is on: the screen for it
    /// ("dictation" or "transcription") starts recording once it's up.
    var startWhenOpen: String?
    /// Transcript text size, an index into `FONT_SCALES`.
    var fontStep: Int { didSet { defaults.set(fontStep, forKey: "font_step") } }
    /// The input the mic source is recording from, while it runs.
    var micName: String?
    /// A live recording is open (capturing or paused).
    var recording = false
    var paused = false
    /// Stopped, but passes 2 and 3 are still draining.
    var finishing = false
    var file: FileJob?
    var level: Float = 0
    /// The open thread's transcript.
    var paragraphs: [Para] = []
    /// The open thread.
    var thread = ThreadInfo(id: 0, title: "", last: "", preview: "", count: 0)
    /// The open thread's kept audio: file to the recording it's part of.
    var threadAudio: [String: String] = [:]
    /// Every thread, the most recently active first.
    var threadList: [ThreadInfo] = []
    /// A message for the user (a file that couldn't be read…).
    var notice: String?

    /// The open thread's recordings, oldest first.
    var recordings: [String] { Array(Set(threadAudio.values)).sorted() }

    /// Paragraph id to its stored row, as last written.
    @ObservationIgnored private var written: [String: Saved] = [:]
    @ObservationIgnored private var loaded: Handle?
    @ObservationIgnored private var sources: [Source] = []
    @ObservationIgnored private var saver: Task<Void, Never>?
    @ObservationIgnored private var pendingFile: URL?
    @ObservationIgnored private let clock = RecordingClock()
    @ObservationIgnored private let cancelFile = Flag()
    /// A Dictation holds the mic and the models.
    @ObservationIgnored private(set) var dictating = false

    private init() {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        threads = ThreadStore(path: support.appendingPathComponent("scribe.db").path)
        boost = Boost(rawValue: defaults.string(forKey: "boost") ?? "") ?? .auto
        route = MicRoute(rawValue: defaults.string(forKey: "route") ?? "") ?? .auto
        keepAudio = defaults.bool(forKey: "keep_audio")
        keepNonSpeech = defaults.bool(forKey: "keep_non_speech")
        autoStart = defaults.bool(forKey: "auto_start")
        fontStep = min(max(defaults.object(forKey: "font_step") as? Int ?? FONT_SCALES.count / 2, 0), FONT_SCALES.count - 1)
        phase = models.ready() ? .unloaded : .needModels(error: nil, resume: models.started())
        try? FileManager.default.createDirectory(at: audioDir, withIntermediateDirectories: true)
        show(threads.current())
    }

    func busy() -> Bool { recording || finishing || file != nil }

    /// The loaded models for a Dictation, marking them in use; nil if not loaded or Scribe is busy.
    func claimForDictation() -> Handle? {
        guard let loaded, !busy(), !dictating else { return nil }
        dictating = true
        return loaded
    }

    func releaseFromDictation() { dictating = false }

    /// Where kept recordings live.
    var audioDir: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("audio")
    }

    // ---- models --------------------------------------------------------

    /// The app came to the front: load the models if they're installed,
    /// and pick up a download left running in the background.
    func onVisible() {
        if phase == .unloaded { load() }
        if case .needModels(nil, true) = phase { install() }
    }

    func install() {
        phase = .installing(.downloading(bundle: BUNDLES[0].dir, done: 0, total: BUNDLES[0].approxBytes), 0)
        let total = Double(BUNDLES.map(\.approxBytes).reduce(0, +))
        models.install(progress: { step in
            Task { @MainActor in
                guard case .installing = self.phase else { return }
                let name: String
                let part: Double
                switch step {
                case .downloading(let b, let done, let t):
                    name = b
                    part = Double(done) / Double(max(t, 1)) * 0.9
                case .unpacking(let b, let f):
                    name = b
                    part = 0.9 + f * 0.1
                }
                // Overall: bundles already in, plus this one's share.
                var before: Int64 = 0
                var size: Int64 = 0
                for b in BUNDLES {
                    if b.dir == name {
                        size = b.approxBytes
                        break
                    }
                    before += b.approxBytes
                }
                self.phase = .installing(step, min((Double(before) + part * Double(size)) / total, 1))
            }
        }, done: { error in
            Task { @MainActor in
                if let error {
                    self.phase = .needModels(error: error.localizedDescription, resume: true)
                } else {
                    self.phase = .unloaded
                    self.load()
                }
            }
        })
    }

    func load() {
        if phase == .loading || loaded != nil { return }
        phase = .loading
        let dir = models.dir.path
        Task.detached(priority: .userInitiated) {
            let t0 = Date()
            let result = Result { try Native.load(dir, threads: THREADS) }
            await MainActor.run {
                switch result {
                case .success(let m):
                    self.loaded = m
                    self.phase = .ready
                    print(String(format: "models loaded in %.1f s, memory %d MB", Date().timeIntervalSince(t0), footprintMB()))
                    if let url = self.pendingFile {
                        self.pendingFile = nil
                        self.startFile(url)
                    }
                    self.testHooks()
                case .failure(let e):
                    self.phase = .failed(e.localizedDescription)
                }
            }
        }
    }

    /// For testing without touch (the Simulator, CI): launched with
    /// `-transcribe_at_launch NAME`, transcribes Documents/NAME; with
    /// `-play_at_launch YES` too, then plays it from the start.
    private func testHooks() {
        let args = UserDefaults(suiteName: nil)
        guard let name = args?.volatileDomain(forName: UserDefaults.argumentDomain)["transcribe_at_launch"] as? String else { return }
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        notice = transcribe(docs.appendingPathComponent(name))
        guard args?.volatileDomain(forName: UserDefaults.argumentDomain)["play_at_launch"] != nil else { return }
        Task {
            while file != nil { try? await Task.sleep(nanoseconds: 200_000_000) }
            if let rec = recordings.first { playFromStart(rec) }
        }
    }

    /// Free the models if nothing is using them (low memory); they reload
    /// when the app is next opened.
    func release() {
        guard let m = loaded, !busy(), !dictating else { return }
        Native.freeModels(m)
        loaded = nil
        phase = .unloaded
        print("models released")
    }

    // ---- sources -------------------------------------------------------

    enum Kind: String { case mic, file }

    /// One engine session and what feeds it. `audio`: the file this
    /// source's audio is kept in, part of `recording`.
    final class Source: @unchecked Sendable {
        let kind: Kind
        let label: String?
        let session: Handle
        let audio: String?
        let recording: String?
        /// Set once the first audio arrives (audio thread).
        let started = Flag()
        /// Cut stretches (session ms): their words are left out of this session's paragraphs.
        var cuts: [ClosedRange<Int64>] = []
        /// Epoch ms at the session's time zero, for ordering its paragraphs.
        var baseMs: Int64 = 0
        /// Paragraphs this session produced (to apply its final transcript).
        var ids = Set<String>()
        var capture: MicCapture?
        let polling = Flag()
        let pollerDone = DispatchSemaphore(value: 0)
        var level: Float = 0
        /// Files: audio through pass 2, for progress and decode throttling.
        let doneMs = Counter()

        init(kind: Kind, label: String?, session: Handle, audio: String?, recording: String?) {
            self.kind = kind
            self.label = label
            self.session = session
            self.audio = audio
            self.recording = recording
        }
    }

    /// Start a session for `kind`; with `recording`, its audio is kept as part of that recording.
    private func open(_ kind: Kind, label: String?, streaming: Bool, recording: String?) throws -> Source {
        guard let loaded else { throw NativeError(message: "models not loaded") }
        var audio = recording.map { "\($0)-\(kind.rawValue).opus" }
        var session: Handle
        if let a = audio {
            do {
                session = try Native.start(loaded, rate: UInt32(RATE), streaming: streaming, opusPath: audioDir.appendingPathComponent(a).path)
            } catch {
                print("can't keep audio: \(error.localizedDescription)")
                audio = nil
                session = try Native.start(loaded, rate: UInt32(RATE), streaming: streaming, opusPath: nil)
            }
        } else {
            session = try Native.start(loaded, rate: UInt32(RATE), streaming: streaming, opusPath: nil)
        }
        let src = Source(kind: kind, label: label, session: session, audio: audio, recording: recording)
        if let audio, let recording { threads.addAudio(thread.id, recording: recording, file: audio) }
        sources.append(src)
        Thread.detachNewThread {
            while !src.polling.value {
                let events = Native.poll(src.session, timeoutMs: 100)
                if events.count > 2 {
                    Task { @MainActor in self.apply(src, events) }
                }
            }
            src.pollerDone.signal()
        }
        return src
    }

    /// Stop polling, then free the session. Off the main thread.
    nonisolated private static func close(_ src: Source) {
        src.polling.set()
        src.pollerDone.wait()
        Native.free(src.session)
    }

    // ---- live recording ------------------------------------------------

    /// Start a recording from the mic. Caller has the mic permission.
    func start() {
        guard loaded != nil, !busy(), !dictating else { return }
        Playback.shared.stop()
        sources = []
        let rec = keepAudio ? newRecording() : nil
        let src: Source
        do {
            src = try open(.mic, label: nil, streaming: true, recording: rec)
        } catch {
            notice = error.localizedDescription
            return
        }
        recording = true
        paused = false
        clock.begin()
        startCapture(src)
        UIApplication.shared.isIdleTimerDisabled = true
    }

    private func startCapture(_ src: Source) {
        guard src.capture == nil else { return }
        let clock = clock
        let threads = threads
        let capture = MicCapture(boost: boost, route: route, onName: { name in
            Task { @MainActor in self.micName = name }
        }) { buf, n in
            if !src.started.value {
                // Where it starts in the recording, so playback lines up.
                src.started.set()
                let at = clock.elapsedMs()
                src.baseMs = clock.wallStart + at
                if let audio = src.audio {
                    Task { @MainActor in threads.setAudioOffset(audio, at) }
                }
            }
            Native.push(src.session, buf, n)
        }
        do {
            try capture.start()
            src.capture = capture
        } catch {
            notice = "Microphone: \(error.localizedDescription)"
        }
    }

    private func stopCaptures() {
        for src in sources {
            src.capture?.stop()
            src.capture = nil
        }
    }

    /// Stop listening but keep the recording open.
    func pause() {
        guard recording, !paused, !finishing else { return }
        paused = true
        clock.pause()
        stopCaptures()
        level = 0
        // Close the utterances that were cut off.
        sources.forEach { Native.breakParagraph($0.session) }
    }

    func resume() {
        guard recording, paused, !finishing else { return }
        paused = false
        clock.resume()
        sources.forEach(startCapture)
    }

    /// Elapsed time of the open recording, excluding pauses.
    func elapsedMs() -> Int64 { clock.elapsedMs() }

    private func newRecording() -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        return f.string(from: Date()) + "-" + String(Int(Date().timeIntervalSince1970 * 1e6) % 46_656, radix: 36)
    }

    /// Stop the recording (or cancel a file), then let the passes finish in the background.
    func stop() {
        if file != nil {
            cancelFile.set()
            return
        }
        guard recording, !finishing else { return }
        if !paused { clock.pause() }
        finishing = true
        stopCaptures()
        let srcs = sources
        let dir = audioDir
        UIApplication.shared.isIdleTimerDisabled = false
        // Each session must be finished and freed exactly once.
        Task.detached(priority: .userInitiated) {
            await MainActor.run { self.recording = false }
            let finals = srcs.map { ($0, Native.finish($0.session)) }
            srcs.forEach(Scribe.close)
            let lengths = srcs.compactMap { s in s.audio.map { ($0, Scribe.audioLength(dir, $0)) } }
            await MainActor.run {
                for (f, len) in lengths { self.threads.setAudioLength(f, len) }
                for (src, json) in finals { self.applyFinal(src, json) }
                self.sources = []
                self.level = 0
                self.paused = false
                self.finishing = false
                self.micName = nil
                self.save(now: true)
                if !self.keepNonSpeech { self.dropIfWordless(srcs) }
            }
        }
    }

    // ---- files ---------------------------------------------------------

    /// Transcribe a shared or opened file into a new thread. A message for
    /// the user when it can't start now.
    func transcribe(_ url: URL) -> String? {
        if busy() || pendingFile != nil { return "Scribe is busy; stop the current recording first." }
        switch phase {
        case .needModels, .installing: return "Download the speech models first."
        default: break
        }
        // A private copy: the grant to read it may not last.
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let copy = dir.appendingPathComponent(url.lastPathComponent)
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try FileManager.default.copyItem(at: url, to: copy)
        } catch {
            return "Could not read \(url.lastPathComponent): \(error.localizedDescription)"
        }
        if loaded == nil {
            pendingFile = copy
            load()
            return nil
        }
        startFile(copy)
        return nil
    }

    private func startFile(_ copy: URL) {
        let name = copy.lastPathComponent
        Playback.shared.stop()
        // Each file gets a thread of its own, named after it.
        switchTo(threads.create(name), force: true)
        let src: Source
        do {
            src = try open(.file, label: nil, streaming: false, recording: keepAudio ? newRecording() : nil)
        } catch {
            notice = error.localizedDescription
            return
        }
        src.baseMs = nowMs()
        cancelFile.reset()
        file = FileJob(name: name, progress: 0)
        let cancel = cancelFile
        let dir = audioDir
        Task.detached(priority: .userInitiated) {
            var fed: Int64 = 0
            var error: Error?
            do {
                try decodeAudio(copy, rate: RATE, durationMs: { ms in
                    Task { @MainActor in self.file?.totalMs = ms }
                }, cancelled: { cancel.value }) { buf, n in
                    // Stay at most FILE_AHEAD_MS ahead of pass 2.
                    while !cancel.value && fed - src.doneMs.value > FILE_AHEAD_MS { usleep(50_000) }
                    Native.push(src.session, buf, n)
                    fed += Int64(n) * 1000 / Int64(RATE)
                }
            } catch let e {
                error = e
                print("decode failed: \(e)")
            }
            try? FileManager.default.removeItem(at: copy.deletingLastPathComponent())
            let json = Native.finish(src.session)
            Scribe.close(src)
            let length = src.audio.map { Scribe.audioLength(dir, $0) }
            await MainActor.run {
                if let a = src.audio, let length { self.threads.setAudioLength(a, length) }
                self.applyFinal(src, json)
                self.sources.removeAll { $0 === src }
                self.file = nil
                self.save(now: true)
                if !self.keepNonSpeech { self.dropIfWordless([src]) }
                if let error { self.notice = "Could not read \(name): \(error.localizedDescription)" }
            }
        }
    }

    // ---- kept audio ----------------------------------------------------

    /// Length of a kept file (ms), 0 if it can't be read.
    nonisolated static func audioLength(_ dir: URL, _ file: String) -> Int64 {
        guard let h = try? Native.playerOpen(dir.appendingPathComponent(file).path) else { return 0 }
        defer { Native.playerFree(h) }
        return Native.playerLength(h)
    }

    /// First to last sound in a kept file (ms), or nil if it's all quiet.
    private func audioSound(_ file: String) -> (Int64, Int64)? {
        guard let h = try? Native.playerOpen(audioDir.appendingPathComponent(file).path) else { return nil }
        defer { Native.playerFree(h) }
        return Native.playerSpan(h, thresholdDb: SOUND_DB, padMs: SOUND_PAD_MS)
    }

    /// The open thread's recordings end to end, oldest first, as one
    /// timeline, each at its full length: each file with where it plays
    /// on it, and where to start each recording from. That's its start,
    /// except for a recording with no words, which starts at its first
    /// sound. Fills in lengths and sound spans not known yet.
    private func timeline() -> ([AudioFile], [String: Int64]) {
        var files: [AudioFile] = []
        var starts: [String: Int64] = [:]
        let heard = Set(paragraphs.compactMap(\.audio))
        var at: Int64 = 0
        let byRecording = Dictionary(grouping: threads.audioRows(thread.id), by: \.recording)
        for rec in byRecording.keys.sorted() {
            let rows = byRecording[rec]!
            // Without "Keep non-speech audio", a recording with no text left isn't played.
            let wordless = !rows.contains { heard.contains($0.file) }
            if !keepNonSpeech && wordless { continue }
            let base = rows.map(\.offsetMs).min() ?? 0
            var end: Int64 = 0
            var sound = Int64.max
            for r in rows {
                let off = r.offsetMs - base
                var len = r.lengthMs
                if len <= 0 {
                    len = Scribe.audioLength(audioDir, r.file)
                    threads.setAudioLength(r.file, len)
                }
                files.append(AudioFile(file: r.file, offsetMs: at + off))
                end = max(end, off + len)
                if wordless {
                    var start: Int64?
                    if r.soundStartMs >= 0 {
                        start = r.soundEndMs > r.soundStartMs ? r.soundStartMs : nil
                    } else {
                        let span = audioSound(r.file)
                        threads.setAudioSound(r.file, span?.0 ?? 0, span?.1 ?? 0)
                        start = span?.0
                    }
                    if let start { sound = min(sound, off + start) }
                }
            }
            starts[rec] = at + (sound == .max ? 0 : sound)
            at += end
        }
        return (files, starts)
    }

    /// What playback jumps over on the timeline of `files`: the open
    /// thread's cut stretches, and without "Keep non-speech audio", any
    /// quiet between words longer than `MAX_GAP_MS` (trimmed to that).
    private func skips(_ files: [AudioFile]) -> [ClosedRange<Int64>] {
        let cuts = threads.cuts(thread.id)
        var out = files.flatMap { f in (cuts[f.file] ?? []).map { (f.offsetMs + $0.lowerBound)...(f.offsetMs + $0.upperBound) } }
        if !keepNonSpeech {
            let offsets = Dictionary(files.map { ($0.file, $0.offsetMs) }, uniquingKeysWith: { a, _ in a })
            let speech = paragraphs.flatMap { p -> [(Int64, Int64)] in
                guard let a = p.audio, let off = offsets[a] else { return [] }
                return p.words.map { (off + $0.startMs, off + $0.endMs) }
            }.sorted { $0.0 < $1.0 }
            let half = MAX_GAP_MS / 2
            var heard: Int64 = -half // the start counts as speech, less half a gap
            for (a, b) in speech {
                if a - heard > MAX_GAP_MS { out.append((heard + half)...(a - half)) }
                heard = max(heard, b)
            }
            // After the last word, nothing left to hear.
            if !speech.isEmpty { out.append((heard + half)...(Int64.max / 4)) }
        }
        return out
    }

    /// Play the open thread's audio, every recording in turn, from the start of `recording`.
    func playFromStart(_ recording: String) {
        guard !busy() else { return }
        let (files, starts) = timeline()
        guard let at = starts[recording] else { return }
        Playback.shared.play(dir: audioDir, files: files, at: at, skips: skips(files), title: thread.title)
    }

    /// Play the open thread's audio, every recording in turn, from `word` of `p`.
    func play(_ p: Para, _ word: Word) {
        guard !busy() else { return }
        let (files, _) = timeline()
        guard let offset = files.first(where: { $0.file == p.audio })?.offsetMs else { return }
        Playback.shared.play(dir: audioDir, files: files, at: offset + word.startMs, skips: skips(files), title: thread.title)
    }

    /// Cut characters `from..<to` of `p`'s text: the words they touch go
    /// from the transcript, and with kept audio, their stretch of it is
    /// skipped on playback (the file stays whole).
    func cut(_ p: Para, from: Int, to: Int) {
        Playback.shared.stop()
        let shown = p.shown
        var at = 0
        var gone: [Int] = []
        for (i, w) in shown.enumerated() {
            if at < to && at + w.count > from { gone.append(i) }
            at += w.count + 1
        }
        guard let first = gone.first, let last = gone.last else { return }
        let text = shown.enumerated().filter { !gone.contains($0.offset) }.map(\.element).joined(separator: " ")
        var words: [Word] = []
        if !p.words.isEmpty {
            let a = p.wordAt(first, shown.count).startMs
            let b = p.wordAt(last, shown.count).endMs
            if let audio = p.audio, b > a { threads.addCut(audio, a, b) }
            // Still being transcribed: the engine's later versions of this
            // paragraph are cut the same way (see `parse`).
            sources.first { $0.ids.contains(p.id) }?.cuts.append(a...max(a, b))
            words = p.words.count == shown.count
                ? p.words.enumerated().filter { !gone.contains($0.offset) }.map(\.element)
                : realign(p.words, text)
        }
        replace(p, text, words)
    }

    /// Replace `p`'s text with `text`; word times carry over where the words match.
    func edit(_ p: Para, _ text: String) {
        guard !busy() else { return }
        let t = text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        if t == p.text { return }
        Playback.shared.stop()
        replace(p, t, p.words.isEmpty || t.isEmpty ? [] : realign(p.words, t))
    }

    private func realign(_ words: [Word], _ text: String) -> [Word] {
        guard let first = words.first, let last = words.last else { return [] }
        return Native.alignWords(words, text, startMs: first.startMs, endMs: last.endMs) ?? []
    }

    /// `p` with new text and word times, or gone if `text` is empty; then stored.
    private func replace(_ p: Para, _ text: String, _ words: [Word]) {
        guard let i = paragraphs.firstIndex(where: { $0.id == p.id }) else { return }
        if text.isEmpty {
            paragraphs.remove(at: i)
        } else {
            var q = p
            q.clips = [ClipUi(text: text, partial: false)]
            q.words = words
            paragraphs[i] = q
        }
        sync()
        // Its recording left without text: gone too, unless non-speech audio is
        // kept. (While recording, the end of the recording decides that.)
        if let a = p.audio, let rec = threadAudio[a], !keepNonSpeech, !busy() {
            let files = Set(threadAudio.filter { $0.value == rec }.keys)
            if !paragraphs.contains(where: { $0.audio.map(files.contains) == true }) {
                files.forEach(threads.removeAudio)
                deleteAudio(Array(files))
                refreshThreads()
            }
        }
    }

    // ---- transcript ----------------------------------------------------

    /// A finished recording's audio, when no paragraph has words in it
    /// ("Keep non-speech audio" off).
    private func dropIfWordless(_ srcs: [Source]) {
        let files = Set(srcs.compactMap(\.audio))
        if files.isEmpty { return }
        if paragraphs.contains(where: { $0.audio.map(files.contains) == true && !$0.words.isEmpty }) { return }
        files.forEach(threads.removeAudio)
        deleteAudio(Array(files))
        refreshThreads()
    }

    private func deleteAudio(_ files: [String]) {
        for f in files { try? FileManager.default.removeItem(at: audioDir.appendingPathComponent(f)) }
    }

    func clear() {
        guard !busy() else { return }
        Playback.shared.stop()
        deleteAudio(threads.clear(thread.id))
        paragraphs = []
        written = [:]
        refreshThreads()
    }

    /// Delete one recording of the open thread: its audio and the text heard in it.
    func deleteRecording(_ recording: String) {
        guard !busy() else { return }
        Playback.shared.stop()
        let files = Set(threadAudio.filter { $0.value == recording }.keys)
        for p in paragraphs where p.audio.map(files.contains) == true {
            if let w = written.removeValue(forKey: p.id) { threads.remove(w.rowId) }
        }
        paragraphs.removeAll { $0.audio.map(files.contains) == true }
        files.forEach(threads.removeAudio)
        deleteAudio(Array(files))
        refreshThreads()
    }

    /// The open thread as text, speakers as "Me: …".
    func text() -> String {
        paragraphs.filter { !$0.text.trimmingCharacters(in: .whitespaces).isEmpty }
            .map { p in p.source.map { "\($0): \(p.text)" } ?? p.text }
            .joined(separator: "\n\n")
    }

    // ---- threads -------------------------------------------------------

    /// Open thread `id` (only while idle, unless `force`).
    func switchTo(_ id: Int64, force: Bool = false) {
        if busy() && !force { return }
        if id == thread.id { return }
        sync()
        dropIfEmpty(thread)
        show(id)
    }

    func newThread() {
        guard !busy() else { return }
        sync()
        // An untouched new thread is reused rather than piling up empty ones.
        if paragraphs.isEmpty && isDateTitle(thread.title) && threadAudio.isEmpty { return }
        dropIfEmpty(thread)
        show(threads.create())
    }

    func rename(_ id: Int64, _ title: String) {
        let t = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        threads.rename(id, t)
        refreshThreads()
    }

    func delete(_ id: Int64) {
        if id == thread.id && busy() { return }
        if id == thread.id { Playback.shared.stop() }
        deleteAudio(threads.delete(id))
        if id == thread.id {
            paragraphs = []
            written = [:]
            thread = ThreadInfo(id: 0, title: "", last: "", preview: "", count: 0)
            show(threads.list().first?.id ?? threads.create())
        } else {
            refreshThreads()
        }
    }

    private func isDateTitle(_ t: String) -> Bool {
        t.range(of: #"^\d{4}-\d\d-\d\d \d\d:\d\d$"#, options: .regularExpression) != nil
    }

    /// Forget a thread left with nothing in it and its date title.
    private func dropIfEmpty(_ t: ThreadInfo) {
        if t.id != 0 && paragraphs.isEmpty && isDateTitle(t.title) && threads.messages(t.id).isEmpty && threads.audioRows(t.id).isEmpty {
            deleteAudio(threads.delete(t.id))
        }
    }

    /// Load thread `id` into `paragraphs` and remember it as the one open.
    private func show(_ id: Int64) {
        Playback.shared.stop()
        saver?.cancel()
        var rows: [String: Saved] = [:]
        paragraphs = threads.messages(id).map { m in
            let p = Para(id: "m\(m.rowId)", clips: [ClipUi(text: m.text, partial: false)], source: m.speaker,
                         audio: m.audio, words: [Word](json: m.words), atMs: m.atMs)
            rows[p.id] = m
            return p
        }
        written = rows
        threads.setCurrent(id)
        thread = ThreadInfo(id: id, title: "", last: "", preview: "", count: 0)
        refreshThreads()
    }

    func refreshThreads() {
        threadList = threads.list()
        if let t = threadList.first(where: { $0.id == thread.id }) { thread = t }
        threadAudio = thread.id != 0
            ? Dictionary(threads.audioRows(thread.id).map { ($0.file, $0.recording) }, uniquingKeysWith: { a, _ in a })
            : [:]
    }

    private func apply(_ src: Source, _ json: Data) {
        guard sources.contains(where: { $0 === src }),
              let events = try? JSONDecoder().decode([RawEvent].self, from: json) else { return }
        var changed = false
        for ev in events {
            switch ev.t {
            case "paragraph":
                guard let o = ev.p else { continue }
                upsert(src, parse(src, o))
                if ev.change != "partial" { changed = true }
                if src.kind == .file { src.doneMs.max(finalEnd(o)) }
            case "removed":
                paragraphs.removeAll { $0.id == ev.id }
                changed = true
            case "level":
                if !paused {
                    src.level = ev.rms ?? 0
                    level = sources.map(\.level).max() ?? 0
                }
            default:
                break
            }
        }
        if src.kind == .file, var job = file {
            let total = job.totalMs
            let done = src.doneMs.value
            job.progress = total > 0 ? min(max(Double(done) / Double(total), 0), 1) : -1
            job.doneMs = min(done, total)
            file = job
        }
        if changed { save(now: false) }
    }

    /// End of the last pass-2 clip in a paragraph.
    private func finalEnd(_ o: RawParagraph) -> Int64 {
        o.clips.filter { !$0.partial }.map { $0.start_ms + ($0.duration_ms ?? 0) }.max() ?? 0
    }

    private func applyFinal(_ src: Source, _ json: Data) {
        let list = (try? JSONDecoder().decode(RawTranscript.self, from: json))?.paragraphs ?? []
        let final = list.map { parse(src, $0) }
        let keep = Set(final.map(\.id))
        paragraphs.removeAll { src.ids.contains($0.id) && !keep.contains($0.id) }
        final.forEach { upsert(src, $0) }
    }

    /// Add or replace `p`, in the order it was said: a paragraph can be
    /// finished after a later one.
    private func upsert(_ src: Source, _ p: Para) {
        src.ids.insert(p.id)
        let i = paragraphs.firstIndex { $0.id == p.id }
        if p.text.isEmpty && !p.partial {
            // All of it cut.
            if let i { paragraphs.remove(at: i) }
            return
        }
        if let i, paragraphs[i].atMs == p.atMs {
            paragraphs[i] = p
            return
        }
        if let i { paragraphs.remove(at: i) }
        if let at = paragraphs.firstIndex(where: { $0.atMs > p.atMs }) {
            paragraphs.insert(p, at: at)
        } else {
            paragraphs.append(p)
        }
    }

    private func parse(_ src: Source, _ o: RawParagraph) -> Para {
        let p = Para(
            id: o.id,
            clips: o.clips.map { ClipUi(text: $0.text, partial: $0.partial) },
            source: src.label,
            audio: src.audio,
            words: o.words ?? [],
            atMs: src.baseMs + o.start_ms
        )
        return withoutCuts(src, p)
    }

    /// `p` without the words `src` has had cut (finished text only).
    private func withoutCuts(_ src: Source, _ p: Para) -> Para {
        if src.cuts.isEmpty || p.partial { return p }
        let shown = p.shown
        if p.words.count != shown.count { return p }
        let keep = p.words.indices.filter { i in !src.cuts.contains { $0.contains(p.words[i].startMs) } }
        if keep.count == shown.count { return p }
        var q = p
        q.clips = [ClipUi(text: keep.map { shown[$0] }.joined(separator: " "), partial: false)]
        q.words = keep.map { p.words[$0] }
        return q
    }

    /// Store the open thread's finished text (partials are left out), so
    /// a killed app doesn't lose it. Debounced unless `now`.
    private func save(now: Bool) {
        saver?.cancel()
        if now {
            sync()
            return
        }
        saver = Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if !Task.isCancelled { sync() }
        }
    }

    /// Write what changed in `paragraphs` since the last sync.
    func sync() {
        saver?.cancel()
        let id = thread.id
        guard id != 0 else { return }
        var present = Set<String>()
        for p in paragraphs {
            let text = p.clips.filter { !$0.partial && !$0.text.trimmingCharacters(in: .whitespaces).isEmpty }.map(\.text).joined(separator: " ")
            if text.isEmpty { continue }
            present.insert(p.id)
            let words = p.words.isEmpty ? nil : p.words.json
            if let w = written[p.id] {
                if w.text != text || w.words != words || w.atMs != p.atMs {
                    threads.edit(w.rowId, text, words: words, atMs: p.atMs)
                    written[p.id] = Saved(rowId: w.rowId, text: text, speaker: w.speaker, audio: w.audio, words: words, atMs: p.atMs)
                }
            } else {
                let row = threads.say(id, text, speaker: p.source, audio: p.audio, words: words, atMs: p.atMs)
                written[p.id] = Saved(rowId: row, text: text, speaker: p.source, audio: p.audio, words: words, atMs: p.atMs)
            }
        }
        for k in written.keys where !present.contains(k) {
            if let w = written.removeValue(forKey: k) { threads.remove(w.rowId) }
        }
        refreshThreads()
    }
}

/// The open recording's clock, read from the audio thread too.
final class RecordingClock: @unchecked Sendable {
    private let lock = NSLock()
    private var active: Int64 = 0
    private var runStart = Date()
    private var running = false
    private var wall: Int64 = 0

    /// Wall clock (epoch ms) when the recording started.
    var wallStart: Int64 { lock.withLock { wall } }

    func begin() {
        lock.withLock {
            active = 0
            runStart = Date()
            running = true
            wall = nowMs()
        }
    }

    func pause() {
        lock.withLock {
            if running { active += Int64(Date().timeIntervalSince(runStart) * 1000) }
            running = false
        }
    }

    func resume() {
        lock.withLock {
            runStart = Date()
            running = true
        }
    }

    /// Recorded time, excluding pauses.
    func elapsedMs() -> Int64 {
        lock.withLock { active + (running ? Int64(Date().timeIntervalSince(runStart) * 1000) : 0) }
    }
}

/// A flag set from one thread and read on others.
final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var on = false
    var value: Bool { lock.withLock { on } }
    func set() { lock.withLock { on = true } }
    func reset() { lock.withLock { on = false } }
}

/// A count shared between threads.
final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var n: Int64 = 0
    var value: Int64 { lock.withLock { n } }
    func max(_ k: Int64) { lock.withLock { if k > n { n = k } } }
    func add(_ k: Int64) { lock.withLock { n += k } }
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
