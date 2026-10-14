import Foundation

/// Swift face of crates/vox_ios (see vox_ios.h). Handles are opaque
/// pointers; failures come back as `NativeError` with Rust's message.
struct NativeError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// A Rust-owned handle, passed between threads; the callers own the order
/// of calls, as on Android.
struct Handle: @unchecked Sendable, Equatable {
    let ptr: OpaquePointer
}

enum Native {
    /// Runs `body` with an error slot; throws its message when it fails.
    private static func checked<T>(_ body: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> T?) throws -> T {
        var err: UnsafeMutablePointer<CChar>?
        if let v = body(&err) {
            vox_string_free(err)
            return v
        }
        let message = err.map { String(cString: $0) } ?? "failed"
        vox_string_free(err)
        throw NativeError(message: message)
    }

    private static func take(_ s: UnsafeMutablePointer<CChar>?) -> String? {
        guard let s else { return nil }
        defer { vox_string_free(s) }
        return String(cString: s)
    }

    /// Extracts entries of a `.tar.bz2` into `dest`; `spec` has one
    /// `<path below the top folder>\t<installed name>` per line. Blocks.
    static func unpack(_ archive: String, _ dest: String, _ spec: String) throws {
        _ = try checked { err in vox_unpack(archive, dest, spec, err) ? true : nil }
    }

    /// Compressed bytes the running `unpack` has read.
    static func unpackProgress() -> UInt64 { vox_unpack_progress() }

    /// Parakeet + Zipformer from `dir`. Slow (seconds).
    static func load(_ dir: String, threads: Int32) throws -> Handle {
        Handle(ptr: try checked { err in vox_load(dir, threads, err) })
    }

    static func freeModels(_ models: Handle) { vox_free_models(models.ptr) }

    /// One session (one source) fed at `rate` Hz mono; `streaming` adds
    /// live partials. With `opusPath` the pushed audio is also kept there
    /// as Ogg Opus, closed by `finish`.
    static func start(_ models: Handle, rate: UInt32, streaming: Bool, opusPath: String?) throws -> Handle {
        Handle(ptr: try checked { err in
            if let opusPath {
                return opusPath.withCString { vox_start(models.ptr, rate, streaming, $0, err) }
            }
            return vox_start(models.ptr, rate, streaming, nil, err)
        })
    }

    static func push(_ session: Handle, _ samples: UnsafePointer<Float>, _ count: Int) {
        vox_push(session.ptr, samples, count)
    }

    static func push(_ session: Handle, _ samples: [Float]) {
        samples.withUnsafeBufferPointer { push(session, $0.baseAddress!, $0.count) }
    }

    static func breakParagraph(_ session: Handle) { vox_break_paragraph(session.ptr) }

    /// JSON array of events, waiting up to `timeoutMs` for the first.
    static func poll(_ session: Handle, timeoutMs: Int32) -> Data {
        Data((take(vox_poll(session.ptr, timeoutMs)) ?? "[]").utf8)
    }

    /// Drains every pass; JSON `{"paragraphs": [...]}`. Blocks. `poll`
    /// and `push` stay safe meanwhile (pushes are dropped); `free` after.
    static func finish(_ session: Handle) -> Data {
        Data((take(vox_finish(session.ptr)) ?? "{}").utf8)
    }

    static func free(_ session: Handle) { vox_free(session.ptr) }

    /// Timings for the words of `text`, an edit of the text `words`
    /// timed: surviving words keep their times, new ones are spread
    /// between, within `startMs...endMs`.
    static func alignWords(_ words: [Word], _ text: String, startMs: Int64, endMs: Int64) -> [Word]? {
        guard let json = try? String(data: JSONEncoder().encode(words), encoding: .utf8),
              let out = take(vox_align_words(json, text, UInt64(max(startMs, 0)), UInt64(max(endMs, 0))))
        else { return nil }
        return try? JSONDecoder().decode([Word].self, from: Data(out.utf8))
    }

    /// An Ogg Opus recording for playback, at its start: mono float, 48 kHz.
    static func playerOpen(_ path: String) throws -> Handle {
        Handle(ptr: try checked { err in vox_player_open(path, err) })
    }

    /// From the first to the last 50 ms louder than `thresholdDb` dBFS,
    /// padded by `padMs`; nil if none is. Rewinds the player.
    static func playerSpan(_ player: Handle, thresholdDb: Float, padMs: Int64) -> (Int64, Int64)? {
        var a: UInt64 = 0
        var b: UInt64 = 0
        guard vox_player_span(player.ptr, thresholdDb, UInt64(padMs), &a, &b) else { return nil }
        return (Int64(a), Int64(b))
    }

    static func playerLength(_ player: Handle) -> Int64 { Int64(vox_player_length(player.ptr)) }

    @discardableResult
    static func playerSeek(_ player: Handle, _ ms: Int64) -> Bool { vox_player_seek(player.ptr, UInt64(max(ms, 0))) }

    /// Fills `out` with up to `count` samples; how many, fewer only at the end.
    static func playerRead(_ player: Handle, _ out: UnsafeMutablePointer<Float>, _ count: Int) -> Int {
        vox_player_read(player.ptr, out, count)
    }

    static func playerFree(_ player: Handle) { vox_player_free(player.ptr) }
}

/// A word and its span in its recording, in ms (the engine's `words`).
struct Word: Codable, Equatable, Hashable {
    let text: String
    let startMs: Int64
    let endMs: Int64

    enum CodingKeys: String, CodingKey {
        case text
        case startMs = "start_ms"
        case endMs = "end_ms"
    }
}

extension Array where Element == Word {
    var json: String { (try? String(data: JSONEncoder().encode(self), encoding: .utf8)) ?? "[]" }

    init(json: String?) {
        guard let json, let w = try? JSONDecoder().decode([Word].self, from: Data(json.utf8)) else {
            self = []
            return
        }
        self = w
    }
}

/// The engine's JSON, as both the transcript and the dictations read it.
struct RawClip: Decodable {
    let id: String
    let start_ms: Int64
    let duration_ms: Int64?
    let text: String
    let stage: String
    let words: [Word]?

    var partial: Bool { stage == "partial" }
}

struct RawParagraph: Decodable {
    let id: String
    let start_ms: Int64
    let text: String
    let clips: [RawClip]
    let words: [Word]?
}

struct RawEvent: Decodable {
    let t: String
    let change: String?
    let p: RawParagraph?
    let id: String?
    let rms: Float?
    let speaking: Bool?
}

struct RawTranscript: Decodable {
    let paragraphs: [RawParagraph]
}
