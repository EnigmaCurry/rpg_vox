import Foundation
import SQLite3

/// A thread in the list: `last` is local "YYYY-MM-DD HH:MM" of its latest text (or creation).
struct ThreadInfo: Identifiable, Equatable {
    let id: Int64
    let title: String
    let last: String
    let preview: String
    let count: Int
}

/// A dictation: `updated` is local "YYYY-MM-DD HH:MM" of its last change.
struct DictationInfo: Identifiable, Equatable {
    let id: Int64
    let title: String
    let updated: String
    let text: String
}

/// One stored paragraph. `audio` is the recording it was heard in and
/// `words` its word timings in that recording (JSON). `atMs` is when it
/// was said (epoch ms), which orders a thread.
struct Saved {
    let rowId: Int64
    let text: String
    let speaker: String?
    var audio: String?
    var words: String?
    var atMs: Int64 = 0
}

/// One source's file of `recording`, `offsetMs` into it and `lengthMs`
/// long (0: not known yet), with sound from `soundStartMs` to
/// `soundEndMs` (-1: not looked for yet).
struct AudioRow {
    let file: String
    let recording: String
    let offsetMs: Int64
    let lengthMs: Int64
    let soundStartMs: Int64
    let soundEndMs: Int64
}

private let iso: ISO8601DateFormatter = {
    let f = ISO8601DateFormatter()
    f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return f
}()

/// The desktop's `at` format, from epoch ms.
private func isoAt(_ ms: Int64) -> String {
    iso.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
}

private func parseAt(_ at: String?) -> Int64 {
    guard let at, let d = iso.date(from: at) else { return 0 }
    return Int64((d.timeIntervalSince1970 * 1000).rounded())
}

/// Local "yyyy-MM-dd HH:mm", the desktop's title for a new thread.
func dateTitle(_ date: Date = Date()) -> String {
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.dateFormat = "yyyy-MM-dd HH:mm"
    return f.string(from: date)
}

private let SQLITE_TRANSIENT = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

/// A value bound to a statement parameter.
enum SQLValue {
    case int(Int64)
    case text(String)
    case null

    static func opt(_ s: String?) -> SQLValue { s.map(SQLValue.text) ?? .null }
}

/// Threads (named transcripts) in `scribe.db`, with the same schema as
/// the Android app's ThreadStore (the desktop chat database's, crates/
/// vox_chat, plus the app's additions): `conversations` are threads, each
/// finished paragraph is a `messages` row (`role='user'`, `kind='say'`),
/// `settings.conversation` is the thread last open, and `messages.at` is
/// when a paragraph was said. Kept audio: `messages.audio`/`words` and
/// the `audio` table, which groups the files a recording made with each
/// one's start in it; `cuts` are stretches skipped on playback. Plus
/// `dictations`. Main thread only.
final class ThreadStore {
    private var db: OpaquePointer?

    init(path: String) {
        guard sqlite3_open(path, &db) == SQLITE_OK else {
            fatalError("open \(path): \(String(cString: sqlite3_errmsg(db)))")
        }
        exec("PRAGMA journal_mode=WAL")
        exec("""
            CREATE TABLE IF NOT EXISTS conversations (
                id         INTEGER PRIMARY KEY,
                title      TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            )
            """)
        exec("""
            CREATE TABLE IF NOT EXISTS messages (
                id       INTEGER PRIMARY KEY,
                conversation INTEGER REFERENCES conversations(id),
                role     TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
                kind     TEXT    NOT NULL DEFAULT 'say' CHECK (kind IN ('say', 'interrupt')),
                reply_to INTEGER REFERENCES messages(id),
                text     TEXT    NOT NULL DEFAULT '',
                more     INTEGER NOT NULL DEFAULT 0,
                at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            )
            """)
        exec("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        for column in ["speaker", "audio", "words"] where !hasColumn("messages", column) {
            exec("ALTER TABLE messages ADD COLUMN \(column) TEXT")
        }
        exec("""
            CREATE TABLE IF NOT EXISTS audio (
                file         TEXT PRIMARY KEY,
                conversation INTEGER REFERENCES conversations(id),
                recording    TEXT NOT NULL,
                offset_ms    INTEGER NOT NULL DEFAULT 0
            )
            """)
        exec("CREATE TABLE IF NOT EXISTS cuts (file TEXT NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL)")
        exec("""
            CREATE TABLE IF NOT EXISTS dictations (
                id         INTEGER PRIMARY KEY,
                title      TEXT NOT NULL,
                text       TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            )
            """)
        for (column, def) in [("length_ms", 0), ("sound_start_ms", -1), ("sound_end_ms", -1)] where !hasColumn("audio", column) {
            exec("ALTER TABLE audio ADD COLUMN \(column) INTEGER NOT NULL DEFAULT \(def)")
        }
    }

    // ---- plumbing ------------------------------------------------------

    private func exec(_ sql: String) {
        if sqlite3_exec(db, sql, nil, nil, nil) != SQLITE_OK {
            print("sqlite: \(String(cString: sqlite3_errmsg(db))) in \(sql)")
        }
    }

    private func prepare(_ sql: String, _ args: [SQLValue]) -> OpaquePointer? {
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else {
            print("sqlite: \(String(cString: sqlite3_errmsg(db))) in \(sql)")
            return nil
        }
        for (i, a) in args.enumerated() {
            let n = Int32(i + 1)
            switch a {
            case .int(let v): sqlite3_bind_int64(stmt, n, v)
            case .text(let v): sqlite3_bind_text(stmt, n, v, -1, SQLITE_TRANSIENT)
            case .null: sqlite3_bind_null(stmt, n)
            }
        }
        return stmt
    }

    /// Runs a statement that returns no rows; the last inserted row id.
    @discardableResult
    private func run(_ sql: String, _ args: SQLValue...) -> Int64 {
        guard let stmt = prepare(sql, args) else { return 0 }
        defer { sqlite3_finalize(stmt) }
        if sqlite3_step(stmt) != SQLITE_DONE {
            print("sqlite: \(String(cString: sqlite3_errmsg(db))) in \(sql)")
        }
        return sqlite3_last_insert_rowid(db)
    }

    /// Each row of a query, read by `row`.
    private func query<T>(_ sql: String, _ args: SQLValue..., row: (Row) -> T) -> [T] {
        guard let stmt = prepare(sql, args) else { return [] }
        defer { sqlite3_finalize(stmt) }
        var out: [T] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            out.append(row(Row(stmt: stmt)))
        }
        return out
    }

    struct Row {
        let stmt: OpaquePointer
        func int(_ i: Int32) -> Int64 { sqlite3_column_int64(stmt, i) }
        func text(_ i: Int32) -> String? {
            sqlite3_column_text(stmt, i).map { String(cString: $0) }
        }
        func string(_ i: Int32) -> String { text(i) ?? "" }
    }

    private func hasColumn(_ table: String, _ column: String) -> Bool {
        !query("SELECT 1 FROM pragma_table_info(?) WHERE name = ?", .text(table), .text(column)) { _ in true }.isEmpty
    }

    private func transaction(_ body: () -> Void) {
        exec("BEGIN")
        body()
        exec("COMMIT")
    }

    // ---- threads -------------------------------------------------------

    /// A new thread titled `title`, or with the local date and time like the desktop's.
    func create(_ title: String? = nil) -> Int64 {
        run("INSERT INTO conversations (title) VALUES (?)", .text(title ?? dateTitle()))
    }

    func rename(_ id: Int64, _ title: String) {
        run("UPDATE conversations SET title = ? WHERE id = ?", .text(title), .int(id))
    }

    /// Delete thread `id`; its audio files, for the caller to remove.
    func delete(_ id: Int64) -> [String] {
        let files = audioFiles(id)
        transaction {
            for f in files { run("DELETE FROM cuts WHERE file = ?", .text(f)) }
            run("DELETE FROM messages WHERE conversation = ?", .int(id))
            run("DELETE FROM audio WHERE conversation = ?", .int(id))
            run("DELETE FROM conversations WHERE id = ?", .int(id))
        }
        return files
    }

    func exists(_ id: Int64) -> Bool {
        !query("SELECT 1 FROM conversations WHERE id = ?", .int(id)) { _ in true }.isEmpty
    }

    /// Every thread, the most recently active first (as the desktop lists them).
    func list() -> [ThreadInfo] {
        query("""
            SELECT c.id, c.title,
                   strftime('%Y-%m-%d %H:%M', coalesce(max(m.at), c.created_at), 'localtime'),
                   (SELECT text FROM messages WHERE conversation = c.id ORDER BY id DESC LIMIT 1),
                   count(m.id)
            FROM conversations c LEFT JOIN messages m ON m.conversation = c.id
            GROUP BY c.id
            ORDER BY coalesce(max(m.at), c.created_at) DESC, c.id DESC
            """) { r in
            ThreadInfo(id: r.int(0), title: r.string(1), last: r.string(2), preview: r.string(3), count: Int(r.int(4)))
        }
    }

    /// The thread last open, else the most recent, else a new one.
    func current() -> Int64 {
        if let saved = setting("conversation").flatMap(Int64.init), exists(saved) { return saved }
        return list().first?.id ?? create()
    }

    func setCurrent(_ id: Int64) { setSetting("conversation", "\(id)") }

    func messages(_ thread: Int64) -> [Saved] {
        query(
            "SELECT id, text, speaker, audio, words, at FROM messages WHERE conversation = ? AND role = 'user' AND kind = 'say' ORDER BY at, id",
            .int(thread)
        ) { r in
            Saved(rowId: r.int(0), text: r.string(1), speaker: r.text(2), audio: r.text(3), words: r.text(4), atMs: parseAt(r.text(5)))
        }
    }

    func say(_ thread: Int64, _ text: String, speaker: String?, audio: String?, words: String?, atMs: Int64) -> Int64 {
        if atMs > 0 {
            return run(
                "INSERT INTO messages (conversation, role, kind, text, speaker, audio, words, at) VALUES (?, 'user', 'say', ?, ?, ?, ?, ?)",
                .int(thread), .text(text), .opt(speaker), .opt(audio), .opt(words), .text(isoAt(atMs))
            )
        }
        return run(
            "INSERT INTO messages (conversation, role, kind, text, speaker, audio, words) VALUES (?, 'user', 'say', ?, ?, ?, ?)",
            .int(thread), .text(text), .opt(speaker), .opt(audio), .opt(words)
        )
    }

    func edit(_ rowId: Int64, _ text: String, words: String?, atMs: Int64) {
        if atMs > 0 {
            run("UPDATE messages SET text = ?, words = ?, at = ? WHERE id = ?", .text(text), .opt(words), .text(isoAt(atMs)), .int(rowId))
        } else {
            run("UPDATE messages SET text = ?, words = ? WHERE id = ?", .text(text), .opt(words), .int(rowId))
        }
    }

    func remove(_ rowId: Int64) {
        run("DELETE FROM messages WHERE id = ?", .int(rowId))
    }

    /// Empty `thread`; its audio files, for the caller to remove.
    func clear(_ thread: Int64) -> [String] {
        let files = audioFiles(thread)
        transaction {
            for f in files { run("DELETE FROM cuts WHERE file = ?", .text(f)) }
            run("DELETE FROM messages WHERE conversation = ?", .int(thread))
            run("DELETE FROM audio WHERE conversation = ?", .int(thread))
        }
        return files
    }

    // ---- kept audio ----------------------------------------------------

    /// Register `file`, one source of `recording` in `thread`.
    func addAudio(_ thread: Int64, recording: String, file: String) {
        run("INSERT OR IGNORE INTO audio (file, conversation, recording) VALUES (?, ?, ?)", .text(file), .int(thread), .text(recording))
    }

    /// Where `file` starts in its recording.
    func setAudioOffset(_ file: String, _ offsetMs: Int64) {
        run("UPDATE audio SET offset_ms = ? WHERE file = ?", .int(offsetMs), .text(file))
    }

    func setAudioLength(_ file: String, _ lengthMs: Int64) {
        run("UPDATE audio SET length_ms = ? WHERE file = ?", .int(lengthMs), .text(file))
    }

    func setAudioSound(_ file: String, _ startMs: Int64, _ endMs: Int64) {
        run("UPDATE audio SET sound_start_ms = ?, sound_end_ms = ? WHERE file = ?", .int(startMs), .int(endMs), .text(file))
    }

    func removeAudio(_ file: String) {
        run("DELETE FROM audio WHERE file = ?", .text(file))
        run("DELETE FROM cuts WHERE file = ?", .text(file))
    }

    /// Skip `startMs...endMs` of `file` on playback.
    func addCut(_ file: String, _ startMs: Int64, _ endMs: Int64) {
        run("INSERT INTO cuts (file, start_ms, end_ms) VALUES (?, ?, ?)", .text(file), .int(startMs), .int(endMs))
    }

    /// `thread`'s cuts, by file.
    func cuts(_ thread: Int64) -> [String: [ClosedRange<Int64>]] {
        var out: [String: [ClosedRange<Int64>]] = [:]
        let rows = query(
            "SELECT c.file, c.start_ms, c.end_ms FROM cuts c JOIN audio a ON a.file = c.file WHERE a.conversation = ?",
            .int(thread)
        ) { r in (r.string(0), r.int(1), r.int(2)) }
        for (f, a, b) in rows where b >= a { out[f, default: []].append(a...b) }
        return out
    }

    /// `thread`'s kept audio, oldest recording first.
    func audioRows(_ thread: Int64) -> [AudioRow] {
        query(
            "SELECT file, recording, offset_ms, length_ms, sound_start_ms, sound_end_ms FROM audio WHERE conversation = ? ORDER BY recording, file",
            .int(thread)
        ) { r in
            AudioRow(file: r.string(0), recording: r.string(1), offsetMs: r.int(2), lengthMs: r.int(3), soundStartMs: r.int(4), soundEndMs: r.int(5))
        }
    }

    private func audioFiles(_ thread: Int64) -> [String] {
        query("SELECT file FROM audio WHERE conversation = ?", .int(thread)) { $0.string(0) }
    }

    // ---- settings ------------------------------------------------------

    func setting(_ key: String) -> String? {
        query("SELECT value FROM settings WHERE key = ?", .text(key)) { $0.text(0) }.first ?? nil
    }

    func setSetting(_ key: String, _ value: String) {
        run("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)", .text(key), .text(value))
    }

    // ---- dictations ----------------------------------------------------

    /// Every dictation, the most recently changed first.
    func dictations() -> [DictationInfo] {
        query("""
            SELECT id, title, strftime('%Y-%m-%d %H:%M', updated_at, 'localtime'), text
            FROM dictations ORDER BY updated_at DESC, id DESC
            """) { r in DictationInfo(id: r.int(0), title: r.string(1), updated: r.string(2), text: r.string(3)) }
    }

    /// A new, empty dictation titled with the local date and time.
    func createDictation() -> Int64 {
        run("INSERT INTO dictations (title) VALUES (?)", .text(dateTitle()))
    }

    func dictation(_ id: Int64) -> DictationInfo? {
        query("""
            SELECT id, title, strftime('%Y-%m-%d %H:%M', updated_at, 'localtime'), text
            FROM dictations WHERE id = ?
            """, .int(id)) { r in DictationInfo(id: r.int(0), title: r.string(1), updated: r.string(2), text: r.string(3)) }.first
    }

    func saveDictation(_ id: Int64, _ text: String) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        run("UPDATE dictations SET text = ?, updated_at = ? WHERE id = ?", .text(text), .text(isoAt(now)), .int(id))
    }

    func renameDictation(_ id: Int64, _ title: String) {
        run("UPDATE dictations SET title = ? WHERE id = ?", .text(title), .int(id))
    }

    func deleteDictation(_ id: Int64) {
        run("DELETE FROM dictations WHERE id = ?", .int(id))
    }
}
