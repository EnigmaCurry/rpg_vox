import Foundation
import Observation
import UIKit

/// One dictation session: an engine session fed by the mic, on models
/// from `Scribe.claimForDictation`. Events arrive on the main thread
/// through `onChange`; `finish` or `cancel` ends it, frees the session and
/// gives the models back.
final class Dictation: @unchecked Sendable {
    private let session: Handle
    private var capture: MicCapture?
    private let polling = Flag()
    private let pollerDone = DispatchSemaphore(value: 0)
    private let ended = Flag()
    /// Samples pushed so far: the session's audio clock.
    private let pushed = Counter()
    private let breakAsked = Flag()
    private var paras: [String: KPara] = [:]
    private var order: [String] = []
    private let onChange: ([KPara]) -> Void
    private let onLevel: (Float) -> Void

    init(models: Handle, onChange: @escaping ([KPara]) -> Void, onLevel: @escaping (Float) -> Void) throws {
        session = try Native.start(models, rate: UInt32(RATE), streaming: true, opusPath: nil)
        self.onChange = onChange
        self.onLevel = onLevel
    }

    @MainActor
    func start(boost: Boost, route: MicRoute) throws {
        let s = session
        Thread.detachNewThread {
            while !self.polling.value {
                let events = Native.poll(s, timeoutMs: 100)
                if events.count > 2 {
                    DispatchQueue.main.async { self.apply(events) }
                }
            }
            self.pollerDone.signal()
        }
        let pushed = pushed
        let breakAsked = breakAsked
        let capture = MicCapture(boost: boost, route: route) { buf, n in
            if breakAsked.value {
                breakAsked.reset()
                Native.breakParagraph(s)
            }
            Native.push(s, buf, n)
            pushed.add(Int64(n))
        }
        do {
            try capture.start()
        } catch {
            cancel()
            throw error
        }
        self.capture = capture
    }

    /// Close the utterance in progress; returns now on the session's clock (ms).
    func breakNow() -> Int64 {
        breakAsked.set()
        return pushed.value * 1000 / Int64(RATE)
    }

    private func apply(_ json: Data) {
        if ended.value { return }
        guard let events = try? JSONDecoder().decode([RawEvent].self, from: json) else { return }
        var changed = false
        for ev in events {
            switch ev.t {
            case "paragraph":
                guard let p = ev.p else { continue }
                if paras[p.id] == nil { order.append(p.id) }
                paras[p.id] = KPara(p)
                changed = true
            case "removed":
                if let id = ev.id, paras.removeValue(forKey: id) != nil {
                    order.removeAll { $0 == id }
                    changed = true
                }
            case "level":
                onLevel(ev.rms ?? 0)
            default:
                break
            }
        }
        // Level events arrive every 50 ms; only text changes redraw.
        if changed { onChange(order.compactMap { paras[$0] }) }
    }

    /// Stop listening, let the passes finish, then hand back the final paragraphs on the main thread.
    func finish(_ done: @escaping ([KPara]) -> Void) {
        end { json in
            let list = (try? JSONDecoder().decode(RawTranscript.self, from: json))?.paragraphs ?? []
            let final = list.map(KPara.init)
            DispatchQueue.main.async { done(final) }
        }
    }

    func cancel() { end { _ in } }

    private func end(_ then: @escaping (Data) -> Void) {
        if ended.value { return }
        ended.set()
        // No push may reach the session once it is finished.
        capture?.stop()
        capture = nil
        let s = session
        Thread.detachNewThread {
            let json = Native.finish(s)
            self.polling.set()
            self.pollerDone.wait()
            Native.free(s)
            DispatchQueue.main.async { Scribe.shared.releaseFromDictation() }
            then(json)
        }
    }
}

/// Typing mode: the text as an ordinary field for the keyboard, with its
/// selection (UTF-16 offsets).
struct Typing: Equatable {
    var text: String
    var selection: NSRange
}

/// Dictations: text written by voice and edited in place, saved, so you
/// can come back and keep going, as in the Android app. Unlike a thread's
/// transcript, which runs in the order things were said, a tap sends new
/// speech anywhere in the text. No audio is kept.
///
/// The open one's text is a `DictationDoc`; a `Dictation` adds to it
/// while listening. Each session folds into the text when it ends, which
/// is saved then and a moment after each change. In `typing` mode the
/// text is a plain field for the keyboard instead, the cursor carried
/// across both ways. Main thread only.
@MainActor @Observable
final class Dictations {
    static let shared = Dictations()

    @ObservationIgnored private var store: ThreadStore { Scribe.shared.threads }

    /// Every dictation, the most recently changed first.
    private(set) var list: [DictationInfo] = []
    /// The dictation on screen, nil for the threads.
    private(set) var open: DictationInfo?
    /// Its text as shown, the cursor standing on `Laid.caret`.
    private(set) var laid = Laid.empty
    private(set) var listening = false
    /// The mic's level while listening (RMS).
    private(set) var level: Float = 0
    /// Stopped, the last words still being transcribed.
    private(set) var finishing = false
    /// What's selected in the text (UTF-16 offsets into `held`'s).
    private(set) var selection = NSRange(location: 0, length: 0)
    /// The text as laid out when the selection began, held while it lasts so new words don't shift it.
    private(set) var held: Laid?
    /// Typing mode, nil when dictating.
    private(set) var typing: Typing?
    /// A message for the user.
    var notice: String?

    @ObservationIgnored private let doc = DictationDoc()
    /// `doc` as last built.
    @ObservationIgnored private var built: [Tok] = []
    @ObservationIgnored private var session: Dictation?
    /// Listening when typing began, to take up again after.
    @ObservationIgnored private var listenAfterTyping = false
    @ObservationIgnored private var saver: Task<Void, Never>?
    /// Runs once the session finishing has saved.
    @ObservationIgnored private var then: (() -> Void)?

    private init() {
        refresh()
        if let id = store.setting("dictation").flatMap(Int64.init), let d = store.dictation(id) { show(d) }
    }

    private func refresh() { list = store.dictations() }

    /// The text as saved.
    func text() -> String {
        typing?.text.trimmingCharacters(in: .whitespacesAndNewlines) ?? doc.text()
    }

    func create() {
        leave {
            self.refresh()
            if let d = self.store.dictation(self.store.createDictation()) { self.show(d) }
        }
    }

    func openDictation(_ id: Int64) {
        if open?.id == id { return }
        leave { if let d = self.store.dictation(id) { self.show(d) } }
    }

    /// Back to the threads.
    func close() {
        leave {
            self.open = nil
            self.store.setSetting("dictation", "")
        }
    }

    func rename(_ id: Int64, _ title: String) {
        let t = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        store.renameDictation(id, t)
        refresh()
        if open?.id == id { open = store.dictation(id) }
    }

    func delete(_ id: Int64) {
        if open?.id == id {
            session?.cancel()
            session = nil
            listening = false
            typing = nil
            open = nil
            store.setSetting("dictation", "")
        }
        store.deleteDictation(id)
        refresh()
    }

    private func show(_ d: DictationInfo) {
        select(NSRange(location: 0, length: 0))
        open = d
        doc.load(d.text)
        relayout()
        store.setSetting("dictation", "\(d.id)")
    }

    /// Finish listening or typing and save, forget the open one if it's empty, then `next`.
    private func leave(_ next: @escaping () -> Void) {
        dictateMode(resume: false)
        let d = open
        let go = {
            if let d, self.store.dictation(d.id)?.text.isEmpty == true {
                self.store.deleteDictation(d.id)
                self.refresh()
            }
            next()
        }
        if session != nil || finishing {
            then = go
            stop()
        } else {
            go()
        }
    }

    /// Start listening, new speech going in at the cursor. Why it can't, or nil.
    func start() -> String? {
        dictateMode(resume: false)
        if open == nil || session != nil || finishing { return nil }
        guard let models = Scribe.shared.claimForDictation() else {
            return Scribe.shared.busy() ? "Scribe is recording; stop it first." : "The speech models aren't loaded yet."
        }
        // A new session's clock starts at zero: fold the last one in first.
        doc.settle()
        do {
            let s = try Dictation(models: models, onChange: { [weak self] paras in
                guard let self else { return }
                self.doc.paras = paras
                self.relayout()
                self.saveSoon()
            }, onLevel: { [weak self] in self?.level = $0 })
            session = s
            try s.start(boost: Scribe.shared.boost, route: Scribe.shared.route)
        } catch {
            session = nil
            Scribe.shared.releaseFromDictation()
            return error.localizedDescription
        }
        listening = true
        UIApplication.shared.isIdleTimerDisabled = true
        relayout()
        return nil
    }

    /// Stop listening; the text is saved once the last words are transcribed.
    func stop() {
        guard let s = session else { return }
        session = nil
        listening = false
        level = 0
        finishing = true
        UIApplication.shared.isIdleTimerDisabled = false
        s.finish { final in
            self.doc.paras = final
            self.doc.settle()
            self.finishing = false
            self.relayout()
            self.save()
            if let t = self.then {
                self.then = nil
                t()
            }
        }
    }

    /// The app left the screen.
    func onHidden() { save() }

    /// Type instead: listening stops, and the text becomes a field for the
    /// keyboard with its cursor where the dictation cursor was.
    func typeMode() {
        if open == nil || typing != nil { return }
        if session != nil { listenAfterTyping = true }
        if session != nil || finishing {
            then = { self.typeMode() }
            stop()
            return
        }
        // The text without the cursor's own space, the cursor where it stood.
        let t = laid.text as NSString
        let c = laid.caret
        var before = c >= 0 ? t.substring(to: c) : t as String
        var after = c >= 0 ? t.substring(from: c + 1) : ""
        while before.hasSuffix(" ") { before.removeLast() }
        while after.hasPrefix(" ") { after.removeFirst() }
        let gap = !before.isEmpty && !after.isEmpty && !before.hasSuffix("\n") && !after.hasPrefix("\n") ? " " : ""
        let at = (before + gap).utf16.count
        typing = Typing(text: before + gap + after, selection: NSRange(location: at, length: 0))
    }

    /// Back to dictating: what was typed is the text, the cursor where the
    /// keyboard's was. If listening when typing began, and `resume`, it
    /// starts again.
    func dictateMode(resume: Bool = true) {
        guard let t = typing else { return }
        typing = nil
        let listen = listenAfterTyping && resume
        listenAfterTyping = false
        let ns = t.text as NSString
        let head = ns.substring(to: min(t.selection.location, ns.length))
        // A cursor within a word counts as after it; one at a line's start
        // as past the break before that line.
        var cursor = DictationDoc.items(head).count
        let rest = ns.substring(from: min(t.selection.location, ns.length))
        if head.trimmingCharacters(in: CharacterSet(charactersIn: " \t")).hasSuffix("\n") && cursor > 0 &&
            !rest.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            cursor += 1
        }
        doc.load(t.text, cursor: cursor)
        relayout()
        save()
        if listen { notice = start() }
    }

    /// Typed: the field's new state.
    func type(_ value: Typing) {
        guard typing != nil else { return }
        typing = value
        saveSoon()
    }

    private func now() -> Int64 { session?.breakNow() ?? 0 }

    /// Send new speech to the gap between words nearest `offset` in `laid`'s text.
    func moveCursor(_ offset: Int) {
        guard let (tok, after) = nearestGap(laid.toks.filter { $0.1.placed }, offset) else { return }
        if doc.moveCursor(built, tok, after: after, now: now) { relayout() }
    }

    /// A new paragraph at the cursor (never two in a row, nor one first).
    func paragraphBreak() {
        if doc.paragraphBreak(built, now: now) { changed() }
    }

    /// The selection in the text changed to `range`.
    func select(_ range: NSRange) {
        selection = range
        held = range.length == 0 ? nil : held ?? laid
    }

    /// ⌫: delete the selection if there is one, else the sentence before
    /// the cursor (or the break right before it).
    func deleteSentence() {
        let sel = selection
        let from = held ?? laid
        select(NSRange(location: 0, length: 0))
        if sel.length > 0 {
            let picked = picked(from, sel)
            if !picked.isEmpty { delete(picked) }
            return
        }
        if doc.deleteSentence(built) { changed() }
    }

    /// The finished words and breaks `sel` touches in `from`.
    func picked(_ from: Laid, _ sel: NSRange) -> [Tok] {
        from.toks.filter { r, t in t.placed && r.lowerBound < sel.upperBound && r.upperBound > sel.location }.map(\.1)
    }

    func delete(_ picked: [Tok]) {
        doc.delete(picked)
        select(NSRange(location: 0, length: 0))
        changed()
    }

    private func changed() {
        relayout()
        saveSoon()
    }

    private func relayout() {
        // Live text is ALL CAPS (Zipformer).
        built = doc.build().map { $0.partial ? $0.withText($0.text.lowercased()) : $0 }
        // A non-breaking space for the cursor to stand on: a plain one would
        // be taken for the space between words.
        laid = doc.lay(built, caret: "\u{00A0}")
    }

    private func saveSoon() {
        saver?.cancel()
        saver = Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if !Task.isCancelled { save() }
        }
    }

    func save() {
        saver?.cancel()
        guard let d = open else { return }
        let text = text()
        if text == store.dictation(d.id)?.text { return }
        store.saveDictation(d.id, text)
        refresh()
        open = store.dictation(d.id)
    }
}
