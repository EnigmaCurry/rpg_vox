import Foundation

/// One sherpa-onnx model bundle: an upstream `.tar.bz2` and the files to
/// keep from it, renamed into `<models>/<dir>/`. Same sources and layout
/// as vox_scribe's `download-models` and the Android app.
struct ModelBundle {
    let dir: String
    let stem: String
    /// Archive path (below the top folder) to installed name.
    let files: [(String, String)]
    /// Approximate archive size, for progress before the server says.
    let approxBytes: Int64

    var url: URL { URL(string: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/\(stem).tar.bz2")! }
}

let BUNDLES = [
    ModelBundle(
        dir: "streaming-zipformer",
        stem: "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17",
        files: [
            ("encoder-epoch-99-avg-1.int8.onnx", "encoder.onnx"),
            ("decoder-epoch-99-avg-1.onnx", "decoder.onnx"),
            ("joiner-epoch-99-avg-1.int8.onnx", "joiner.onnx"),
            ("tokens.txt", "tokens.txt"),
        ],
        approxBytes: 127_887_156
    ),
    ModelBundle(
        dir: "parakeet-tdt-0.6b-v2",
        stem: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        files: ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"].map { ($0, $0) },
        approxBytes: 482_468_385
    ),
]

enum Step: Equatable {
    case downloading(bundle: String, done: Int64, total: Int64)
    case unpacking(bundle: String, fraction: Double)

    var text: String {
        switch self {
        case .downloading(let b, let done, let total):
            "Downloading \(b)… \(done / 1_000_000) / \(total / 1_000_000) MB"
        case .unpacking(let b, let f):
            "Unpacking \(b)… \(Int(f * 100))%"
        }
    }
}

/// The models under Application Support (kept out of backups), fetched
/// with a background URLSession so a download carries on while the app
/// is away, then unpacked by Rust (bzip2 in Swift would be far slower).
/// One bundle at a time; a failed download resumes from where it stopped.
final class ModelStore: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    static let sessionId = "com.enigmacurry.scribe.models"

    let dir: URL
    private let downloads: URL
    /// Set by the app delegate when iOS wakes the app for this session.
    var backgroundDone: (() -> Void)?

    private let lock = NSLock()
    private var session: URLSession!
    private var progress: ((Step) -> Void)?
    private var finished: ((Error?) -> Void)?

    override init() {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        dir = support.appendingPathComponent("models")
        downloads = support.appendingPathComponent("downloads")
        super.init()
        for d in [dir, downloads] {
            try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
            var url = d
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try? url.setResourceValues(values)
        }
        adoptCopied()
        let config = URLSessionConfiguration.background(withIdentifier: Self.sessionId)
        config.isDiscretionary = false
        config.sessionSendsLaunchEvents = true
        session = URLSession(configuration: config, delegate: self, delegateQueue: nil)
    }

    /// Models copied into Documents/models by hand (`just ios-models`,
    /// Finder) move to where the app keeps them.
    private func adoptCopied() {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("models")
        for b in BUNDLES {
            let from = docs.appendingPathComponent(b.dir)
            guard !installed(b), b.files.allSatisfy({ FileManager.default.fileExists(atPath: from.appendingPathComponent($0.1).path) })
            else { continue }
            let to = dir.appendingPathComponent(b.dir)
            try? FileManager.default.removeItem(at: to)
            try? FileManager.default.moveItem(at: from, to: to)
        }
        if (try? FileManager.default.contentsOfDirectory(atPath: docs.path))?.isEmpty == true {
            try? FileManager.default.removeItem(at: docs)
        }
    }

    func installed(_ b: ModelBundle) -> Bool {
        b.files.allSatisfy { FileManager.default.fileExists(atPath: dir.appendingPathComponent("\(b.dir)/\($0.1)").path) }
    }

    func ready() -> Bool { BUNDLES.allSatisfy(installed) }

    /// Some download was left part-way.
    func started() -> Bool {
        !((try? FileManager.default.contentsOfDirectory(atPath: downloads.path)) ?? []).isEmpty
    }

    private func archive(_ b: ModelBundle) -> URL { downloads.appendingPathComponent("\(b.stem).tar.bz2") }
    private func resumeData(_ b: ModelBundle) -> URL { downloads.appendingPathComponent("\(b.stem).resume") }

    /// Fetch and unpack every missing bundle, one after another;
    /// `progress` and `done` are called on a background queue.
    func install(progress: @escaping (Step) -> Void, done: @escaping (Error?) -> Void) {
        lock.withLock {
            self.progress = progress
            self.finished = done
        }
        // A download already running in the background (from before the
        // app was last suspended) carries on; only start one otherwise.
        session.getAllTasks { tasks in
            if tasks.contains(where: { $0.state == .running }) { return }
            self.next()
        }
    }

    /// The next missing bundle: unpack it if fetched, else fetch it.
    private func next() {
        guard let b = BUNDLES.first(where: { !installed($0) }) else {
            try? FileManager.default.removeItem(at: downloads)
            try? FileManager.default.createDirectory(at: downloads, withIntermediateDirectories: true)
            report(nil)
            return
        }
        if FileManager.default.fileExists(atPath: archive(b).path) {
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    try self.unpack(b)
                    self.next()
                } catch {
                    self.report(error)
                }
            }
            return
        }
        let task: URLSessionDownloadTask
        if let data = try? Data(contentsOf: resumeData(b)) {
            try? FileManager.default.removeItem(at: resumeData(b))
            task = session.downloadTask(withResumeData: data)
        } else {
            task = session.downloadTask(with: b.url)
        }
        task.taskDescription = b.dir
        task.resume()
    }

    private func report(_ error: Error?) {
        let done = lock.withLock { () -> ((Error?) -> Void)? in
            defer { finished = nil }
            return finished
        }
        done?(error)
    }

    private func bundle(_ task: URLSessionTask) -> ModelBundle? {
        BUNDLES.first { $0.dir == task.taskDescription }
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData _: Int64,
                    totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
        guard let b = bundle(downloadTask) else { return }
        let total = totalBytesExpectedToWrite > 0 ? totalBytesExpectedToWrite : b.approxBytes
        let p = lock.withLock { progress }
        p?(.downloading(bundle: b.dir, done: totalBytesWritten, total: total))
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        guard let b = bundle(downloadTask) else { return }
        if let http = downloadTask.response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            return
        }
        // The file is gone once this returns: move it now.
        try? FileManager.default.removeItem(at: archive(b))
        try? FileManager.default.moveItem(at: location, to: archive(b))
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let b = bundle(task) else { return }
        if let error {
            let resume = (error as NSError).userInfo[NSURLSessionDownloadTaskResumeData] as? Data
            if let resume { try? resume.write(to: resumeData(b)) }
            report(error)
            return
        }
        if let http = task.response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            report(NativeError(message: "HTTP \(http.statusCode) for \(b.url.absoluteString)"))
            return
        }
        next()
    }

    func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        DispatchQueue.main.async {
            self.backgroundDone?()
            self.backgroundDone = nil
        }
    }

    /// Extract the wanted files into `<dir>.tmp`, then move it into place.
    private func unpack(_ b: ModelBundle) throws {
        let fm = FileManager.default
        let tmp = dir.appendingPathComponent("\(b.dir).tmp")
        try? fm.removeItem(at: tmp)
        try fm.createDirectory(at: tmp, withIntermediateDirectories: true)
        let size = Double(max((try? fm.attributesOfItem(atPath: archive(b).path)[.size] as? Int64) ?? 1, 1))
        let spec = b.files.map { "\($0.0)\t\($0.1)" }.joined(separator: "\n")
        let p = lock.withLock { progress }
        // Progress is compressed bytes read, so skipped entries count too.
        let ticker = DispatchSource.makeTimerSource(queue: .global())
        ticker.schedule(deadline: .now(), repeating: 0.25)
        ticker.setEventHandler {
            p?(.unpacking(bundle: b.dir, fraction: min(Double(Native.unpackProgress()) / size, 1)))
        }
        ticker.resume()
        defer { ticker.cancel() }
        do {
            try Native.unpack(archive(b).path, tmp.path, spec)
        } catch {
            try? fm.removeItem(at: tmp)
            try? fm.removeItem(at: archive(b)) // likely corrupt: fetch it again next time
            throw NativeError(message: "\(b.dir): \(error.localizedDescription)")
        }
        let missing = b.files.map(\.1).filter { !fm.fileExists(atPath: tmp.appendingPathComponent($0).path) }
        if !missing.isEmpty {
            try? fm.removeItem(at: tmp)
            try? fm.removeItem(at: archive(b))
            throw NativeError(message: "\(b.dir): archive lacks \(missing.joined(separator: ", "))")
        }
        let final = dir.appendingPathComponent(b.dir)
        try? fm.removeItem(at: final)
        try fm.moveItem(at: tmp, to: final)
        try? fm.removeItem(at: archive(b))
    }
}
