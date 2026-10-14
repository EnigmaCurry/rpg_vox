import Foundation

/// A paragraph break among the items of a text.
private let BREAK = "\n\n"

/// A dictation's text as the dictation screen edits it, ported from the
/// Android app: a cursor that sends new speech to a gap between words,
/// deleted words, words retyped by hand, and ¶ paragraph breaks. All of
/// it is kept by time on the session's audio clock (ms), so it holds when
/// later passes rewrite the text. The engine's own paragraphs run
/// together; only ¶ breaks them.
///
/// Text from before the session (a saved dictation, see `load`) stands
/// before its time zero, so the same cursor, cuts and edits work on it.
final class DictationDoc {
    /// The engine's latest paragraphs.
    var paras: [KPara] = []
    /// Words from before the session, each at its (negative) time.
    private var base: [(String, Int64)] = []
    /// Spans of the session's audio whose words were deleted.
    private var cuts: [ClosedRange<Int64>] = []
    /// ¶ breaks, each at a time on the session's audio clock.
    private var breaks: [Int64] = []
    /// Words retyped by hand, standing where the first word they replace was said.
    private var edits: [Edit] = []
    /// Cursor moves, in order: speech from each one's `fromMs` on goes there.
    private var inserts: [Insertion] = []
    /// The latest time handed out by `stamp`.
    private var stamped = Int64.min

    func isCut(_ w: Word) -> Bool { cuts.contains { $0.contains(w.startMs) } }

    func shown(_ t: Tok) -> Bool { !t.mark && (t.time.map { !isCut($0) } ?? true) }

    /// `now`, or just after the last time given out if that's later: a
    /// cursor move or break made while the clock stands still must still
    /// come after the one before.
    private func stamp(_ now: Int64) -> Int64 {
        stamped = max(now, stamped == .min ? .min : stamped + 1)
        return stamped
    }

    /// The cursor move whose speech `ms` is, 0 for none (the original run).
    private func segOf(_ ms: Int64) -> Int {
        var seg = 0
        for (i, ins) in inserts.enumerated() where ins.fromMs <= ms { seg = i + 1 }
        return seg
    }

    /// `paras` in reading order, cursor moves applied and seams polished.
    func build() -> [Tok] { polish(document(paras)) }

    /// All the words in reading order. Each cursor move's speech, in the
    /// order said, goes after the word it was placed after, found by time
    /// so later passes rewriting the text don't move it.
    private func document(_ paras: [KPara]) -> [Tok] {
        var toks: [Tok] = base.map { w, ms in Tok(w, Word(text: w, startMs: ms, endMs: ms + 1), ms, false, segOf(ms)) }
        for p in paras {
            for c in p.clips {
                for (i, w) in c.words.enumerated() {
                    let t = i < c.times.count ? c.times[i] : nil
                    let ms = t?.startMs ?? c.startMs
                    toks.append(Tok(w, t, ms, c.partial, segOf(ms), head: i == 0))
                }
            }
        }
        toks += breaks.map { Tok(BREAK, nil, $0, false, segOf($0), brk: true) }
        toks += edits.map { Tok($0.text, nil, $0.ms, false, segOf($0.ms), edit: $0) }
        // Stable: words of one clip keep their order.
        let sorted = toks.enumerated().sorted { a, b in
            a.element.ms != b.element.ms ? a.element.ms < b.element.ms : a.offset < b.offset
        }.map(\.element)
        let bySeg = Dictionary(grouping: sorted, by: \.seg)
        var doc = [Tok.mark(0)] + (bySeg[0] ?? [])
        for (i, ins) in inserts.enumerated() {
            let at: Int
            if let last = doc.lastIndex(where: { !$0.mark && $0.seg == ins.seg && $0.ms <= ins.afterMs }) {
                at = last + 1
            } else {
                at = (doc.firstIndex { $0.mark && $0.seg == ins.seg } ?? -1) + 1
            }
            doc.insert(contentsOf: [Tok.mark(i + 1)] + (bySeg[i + 1] ?? []), at: at)
        }
        return doc
    }

    /// `text`'s words, and a break between each paragraph and the next (a new line starts one).
    static func items(_ text: String) -> [String] {
        var items: [String] = []
        for p in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let words = p.split(whereSeparator: \.isWhitespace).map(String.init)
            if words.isEmpty { continue }
            if !items.isEmpty { items.append(BREAK) }
            items += words
        }
        return items
    }

    /// Start over from `text` (a new line starts a paragraph), nothing
    /// said yet, with the cursor before its `cursor`th word or break, or
    /// at the end.
    func load(_ text: String, cursor: Int = .max) { reset(Self.items(text), cursor) }

    /// Fold everything said and done so far into the text from before,
    /// keeping the cursor where it is: for a new session, whose clock
    /// starts again at zero. Partial words are dropped.
    func settle() {
        let doc = build()
        let cursor = cursorAt(doc)
        var items: [String] = []
        var at = -1
        var lastBrk = true // nor one first
        for (i, t) in doc.enumerated() {
            if i == cursor { at = items.count }
            if !shown(t) || t.partial { continue }
            if t.brk && lastBrk { continue }
            lastBrk = t.brk
            if t.brk {
                items.append(BREAK)
            } else {
                items += t.text.split(separator: " ").map(String.init)
            }
        }
        reset(items, at < 0 ? items.count : at)
    }

    private func reset(_ items: [String], _ cursor: Int) {
        paras = []
        cuts = []
        breaks = []
        edits = []
        inserts = []
        stamped = .min
        // Two ms apart, ending before zero.
        let times = items.indices.map { -2 * Int64(items.count - $0) }
        base = items.indices.filter { items[$0] != BREAK }.map { (items[$0], times[$0]) }
        breaks = items.indices.filter { items[$0] == BREAK }.map { times[$0] }
        // New speech is a cursor move's even at the end, so it's fitted to
        // the sentence it lands in.
        let at = min(max(cursor, 0), items.count)
        inserts.append(Insertion(seg: 0, afterMs: at > 0 ? times[at - 1] : .min, fromMs: 0))
    }

    /// The text as saved: no partial words, paragraphs a blank line apart.
    func text() -> String {
        lay(build().filter { !$0.partial }, caret: "").text
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .joined(separator: "\n")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// `doc` with the seams of cursor moves fixed up: the engine starts
    /// every utterance as a sentence, so speech put mid-sentence begins
    /// with a capital and may end with a full stop. Where the word before
    /// doesn't end a sentence, the first word is lowercased unless it
    /// looks like a name: "I", an acronym, inner capitals, or capitalized
    /// elsewhere mid-sentence. A full stop before a lowercase word of the
    /// old text is dropped. Speech put mid-sentence is a fragment, so the
    /// same goes for the utterances within it.
    private func polish(_ doc: [Tok]) -> [Tok] {
        var out = doc
        let words = doc.indices.filter { shown(doc[$0]) }
        func upper(_ t: Tok) -> Bool { t.text.first?.isUppercase == true }
        func core(_ w: String) -> String { w.trimmingCharacters(in: CharacterSet.alphanumerics.inverted) }
        // Words the engine capitalized mid-utterance after a lowercase word,
        // so likely names. Not after a capital: an utterance's first word is
        // capitalized as a sentence start and can drag the next one along.
        var names = Set<String>()
        for k in words.indices.dropFirst() {
            let prev = doc[words[k - 1]]
            let t = doc[words[k]]
            if !t.brk && !t.partial && !t.head && t.seg == prev.seg && !prev.brk &&
                prev.text.first?.isLowercase == true && upper(t) {
                names.insert(core(t.text))
            }
        }
        func name(_ w: String) -> Bool {
            let c = core(w)
            return c == "I" || c.hasPrefix("I'") || c.hasPrefix("I’") ||
                c.dropFirst().contains { $0.isUppercase } || names.contains(c)
        }
        /// Lowercase the utterance head words[k] and the run of capitals it dragged along.
        func lower(_ k: Int) {
            var m = k
            while m < words.count {
                let t = doc[words[m]]
                if m > k && (t.head || t.brk || t.partial || t.seg != doc[words[k]].seg) { break }
                if t.edit != nil { break } // typed by hand: as typed
                if !upper(t) || name(t.text) { break }
                let s = out[words[m]].text
                out[words[m]] = out[words[m]].withText(s.prefix(1).lowercased() + s.dropFirst())
                m += 1
            }
        }
        func dropStop(_ i: Int) {
            let last = out[i].text
            if last.hasSuffix(".") && !last.hasSuffix("..") { out[i] = out[i].withText(String(last.dropLast())) }
        }
        var fragment = Set<Int>()
        for k in words.indices.dropFirst() {
            let i = words[k - 1]
            let j = words[k]
            let prev = doc[i]
            let t = doc[j]
            if t.brk || prev.brk || t.partial || prev.partial || t.edit != nil || prev.edit != nil { continue }
            let head = t.text
            if t.seg == prev.seg {
                // An utterance within a fragment: carry the sentence on.
                if !t.head || !fragment.contains(t.seg) { continue }
                dropStop(i)
                if !ends(out[i]) { lower(k) }
                continue
            }
            if !ends(prev) {
                fragment.insert(t.seg)
                lower(k)
            }
            // out[i], not prev: the first word may have been lowercased already.
            if head.first?.isLowercase == true { dropStop(i) }
        }
        return out
    }

    /// Where in `doc` new speech goes: after the latest cursor move's words.
    func cursorAt(_ doc: [Tok]) -> Int { (doc.lastIndex { $0.seg == inserts.count } ?? -1) + 1 }

    /// `doc` as text: words a space apart, each shown break once and none
    /// first, and `caret` where the cursor is. Offsets are UTF-16, as
    /// UIKit's.
    func lay(_ doc: [Tok], caret: String) -> Laid {
        var sb = ""
        var len = 0
        var last: Character?
        var toks: [(Range<Int>, Tok)] = []
        var caretAt = -1
        let caretLen = caret.utf16.count
        let cursor = cursorAt(doc)
        func append(_ s: String) {
            sb += s
            len += s.utf16.count
            last = s.last ?? last
        }
        func putCaret() {
            if len > 0 && last != " " && last != "\n" { append(" ") }
            caretAt = len
            append(caret)
        }
        var lastBrk = true // nor one first
        for (i, t) in doc.enumerated() {
            if i == cursor { putCaret() }
            if !shown(t) { continue }
            // Two breaks can't touch, but a break said meanwhile could
            // land beside one; show it once.
            if t.brk && lastBrk { continue }
            lastBrk = t.brk
            if !t.brk && len > 0 && last != " " && last != "\n" && len != caretAt + caretLen { append(" ") }
            if t.brk && len > 0 && last == " " {
                sb.removeLast()
                len -= 1
                last = sb.last
            }
            let at = len
            append(t.text)
            toks.append((at..<len, t))
        }
        if cursor == doc.count { putCaret() }
        return Laid(text: sb, toks: toks, caret: caretAt)
    }

    /// Send new speech to the gap before `tok` (after it if `after`):
    /// `now` finishes what's being said and gives the session's time.
    /// False if the cursor is there already.
    func moveCursor(_ doc: [Tok], _ tok: Tok, after: Bool, now: () -> Int64) -> Bool {
        guard let index = doc.firstIndex(where: { $0 === tok }) else { return false }
        let gap = after ? index + 1 : index
        let cursor = cursorAt(doc)
        // Already there (only hidden words or marks between): nothing moves.
        if !(min(gap, cursor)..<max(gap, cursor)).contains(where: { shown(doc[$0]) }) { return false }
        let from = stamp(now())
        let prev = doc[gap - 1]
        inserts.append(Insertion(seg: prev.seg, afterMs: prev.mark ? .min : prev.ms, fromMs: from))
        return true
    }

    /// A new paragraph at the cursor, `now` finishing what's being said.
    /// Never two in a row, nor one first: false beside a break already
    /// there or with nothing before it.
    func paragraphBreak(_ doc: [Tok], now: () -> Int64) -> Bool {
        let cursor = cursorAt(doc)
        let before = doc[..<cursor].last(where: shown)
        let after = doc[cursor...].first(where: shown)
        guard let before, !before.brk, after?.brk != true else { return false }
        breaks.append(stamp(now()))
        return true
    }

    /// Delete `picked`: heard words are cut, retyped ones and breaks
    /// removed. Returns the cut words' spans of audio.
    @discardableResult
    func delete(_ picked: [Tok]) -> [ClosedRange<Int64>] {
        var spans: [ClosedRange<Int64>] = []
        for t in picked {
            if t.brk {
                if let i = breaks.firstIndex(of: t.ms) { breaks.remove(at: i) }
            } else if let e = t.edit {
                edits.removeAll { $0 === e }
            } else if let w = t.time {
                let r = w.startMs...max(w.startMs, w.endMs - 1)
                cuts.append(r)
                spans.append(r)
            }
        }
        // Breaks now side by side become one, so deleting it later
        // doesn't uncover another.
        var lastBrk = true // nor one first
        for t in document(paras) where shown(t) {
            if t.brk && lastBrk, let i = breaks.firstIndex(of: t.ms) { breaks.remove(at: i) }
            lastBrk = t.brk
        }
        return spans
    }

    /// Delete the sentence before the cursor: back to the end of the one
    /// before it or the paragraph's start. Right after a break, the break
    /// goes instead, joining the paragraphs. False if there's nothing.
    func deleteSentence(_ doc: [Tok]) -> Bool {
        let before = doc[..<cursorAt(doc)].filter { shown($0) && $0.placed }.reversed()
        guard let last = before.first else { return false }
        if last.brk {
            delete([last])
            return true
        }
        var picked = [last]
        for t in before.dropFirst() {
            if ends(t) { break }
            picked.append(t)
        }
        delete(picked)
        return true
    }
}

/// `text` as laid out, where each shown token is in it (UTF-16 offsets),
/// and the cursor's offset (-1: none).
struct Laid {
    let text: String
    let toks: [(Range<Int>, Tok)]
    let caret: Int

    static let empty = Laid(text: "", toks: [], caret: -1)
}

/// The word or break `toks` has nearest `offset`, and whether the gap meant is after it rather than before.
func nearestGap(_ toks: [(Range<Int>, Tok)], _ offset: Int) -> (Tok, Bool)? {
    guard let (at, t) = toks.min(by: { distance($0.0, offset) < distance($1.0, offset) }) else { return nil }
    return (t, offset * 2 > at.lowerBound + at.upperBound)
}

private func distance(_ r: Range<Int>, _ offset: Int) -> Int {
    if offset < r.lowerBound { return r.lowerBound - offset }
    if offset > r.upperBound { return offset - r.upperBound }
    return 0
}

/// A cursor move: speech from `fromMs` on (session audio) goes after the
/// last word of move `seg`'s speech said by `afterMs`, or at its start.
struct Insertion {
    let seg: Int
    let afterMs: Int64
    let fromMs: Int64
}

/// A word of the transcript, `ms` when it was said (its clip's start if
/// untimed), from cursor move `seg`; a `mark` holds where a move's speech
/// starts and shows nothing; a `brk` is a paragraph break. Compared by
/// identity.
final class Tok {
    let text: String
    let time: Word?
    let ms: Int64
    let partial: Bool
    let seg: Int
    let mark: Bool
    let brk: Bool
    /// First word of its clip (utterance).
    let head: Bool
    /// Typed by hand, not heard.
    let edit: Edit?

    init(_ text: String, _ time: Word?, _ ms: Int64, _ partial: Bool, _ seg: Int,
         mark: Bool = false, brk: Bool = false, head: Bool = false, edit: Edit? = nil) {
        self.text = text
        self.time = time
        self.ms = ms
        self.partial = partial
        self.seg = seg
        self.mark = mark
        self.brk = brk
        self.head = head
        self.edit = edit
    }

    func withText(_ text: String) -> Tok {
        Tok(text, time, ms, partial, seg, mark: mark, brk: brk, head: head, edit: edit)
    }

    /// Finished text that can be deleted, retyped, or have the cursor put beside it.
    var placed: Bool { time != nil || brk || edit != nil }

    static func mark(_ seg: Int) -> Tok { Tok("", nil, .min, false, seg, mark: true) }
}

/// Words typed by hand, placed at `ms` on the session's audio clock, for the words said until `endMs`.
final class Edit {
    let ms: Int64
    let text: String
    let endMs: Int64

    init(ms: Int64, text: String, endMs: Int64) {
        self.ms = ms
        self.text = text
        self.endMs = endMs
    }
}

/// `t` ends a sentence (or is a break).
private func ends(_ t: Tok) -> Bool {
    if t.brk { return true }
    var s = Substring(t.text)
    while let c = s.last, "\"')”’".contains(c) { s = s.dropLast() }
    return s.hasSuffix(".") || s.hasSuffix("?") || s.hasSuffix("!") || s.hasSuffix("…")
}

/// A dictation's paragraph as the engine sent it, in the order its clips were said.
struct KPara {
    let id: String
    let clips: [KClip]

    init(_ o: RawParagraph) {
        id = o.id
        clips = o.clips.map(KClip.init)
    }
}

/// One clip's `words` as shown, with their `times` in the session's
/// audio when known (finished clips); a word without a time can't be
/// deleted or have the cursor put beside it. `startMs` is when the clip
/// began.
struct KClip {
    let partial: Bool
    let startMs: Int64
    let words: [String]
    let times: [Word]

    init(_ c: RawClip) {
        partial = c.partial
        startMs = c.start_ms
        words = c.text.split(whereSeparator: \.isWhitespace).map(String.init)
        if partial || words.isEmpty {
            times = []
            return
        }
        let timed = c.words ?? []
        if timed.count == words.count {
            times = timed
        } else {
            // A pass changed the words without timing them: spread the
            // new ones over the clip, keeping the times of those that match.
            let end = c.start_ms + (c.duration_ms ?? 0)
            let aligned = Native.alignWords(timed, c.text, startMs: c.start_ms, endMs: end) ?? []
            times = aligned.count == words.count ? aligned : []
        }
    }
}
