package com.enigmacurry.rpg_vox_scribe

import org.json.JSONObject

/**
 * A dictation's text as the voice keyboard and the app's dictation
 * screen edit it: a cursor that sends new speech to a gap between words,
 * deleted words, words retyped by hand, and ¶ paragraph breaks. All of
 * it is kept by time on the session's audio clock (ms), so it holds when
 * later passes rewrite the text. The engine's own paragraphs run
 * together; only ¶ breaks them.
 *
 * Text from before the session (a saved dictation, see [load]) stands
 * before its time zero, so the same cursor, cuts and edits work on it.
 */
class DictationDoc {
    /** The engine's latest paragraphs. */
    var paras: List<KPara> = emptyList()
    /** Words from before the session, each at its (negative) time. */
    private var base: List<Pair<String, Long>> = emptyList()
    /** Spans of the session's audio whose words were deleted. */
    private val cuts = mutableListOf<LongRange>()
    /** ¶ breaks, each at a time on the session's audio clock. */
    private val breaks = mutableListOf<Long>()
    /** Words retyped by hand, standing where the first word they replace was said. */
    private val edits = mutableListOf<Edit>()
    /** Cursor moves, in order: speech from each one's [Insertion.fromMs] on goes there. */
    private val inserts = mutableListOf<Insertion>()
    /** The latest time handed out by [stamp]. */
    private var stamped = Long.MIN_VALUE

    fun isCut(w: Word) = cuts.any { w.startMs in it }

    fun shown(t: Tok) = !t.mark && (t.time == null || !isCut(t.time))

    /**
     * [now], or just after the last time given out if that's later: a
     * cursor move or break made while paused, when the clock stands
     * still, must still come after the one before.
     */
    private fun stamp(now: Long): Long = maxOf(now, stamped + 1).also { stamped = it }

    /** The cursor move whose speech [ms] is, 0 for none (the original run). */
    private fun segOf(ms: Long): Int {
        var seg = 0
        inserts.forEachIndexed { i, ins -> if (ins.fromMs <= ms) seg = i + 1 }
        return seg
    }

    /** [paras] in reading order, cursor moves applied and seams [polish]ed. */
    fun build(paras: List<KPara> = this.paras): List<Tok> = polish(document(paras))

    /**
     * All the words in reading order. Each cursor move's speech, in the
     * order said, goes after the word it was placed after, found by time
     * so later passes rewriting the text don't move it.
     */
    private fun document(paras: List<KPara>): List<Tok> {
        // sortedBy is stable: words of one clip keep their order.
        val toks = base.map { (w, ms) -> Tok(w, Word(w, ms, ms + 1), ms, false, segOf(ms)) } +
            paras.flatMap { p ->
                p.clips.flatMap { c ->
                    c.words.mapIndexed { i, w ->
                        val t = c.times.getOrNull(i)
                        val ms = t?.startMs ?: c.startMs
                        Tok(w, t, ms, c.partial, segOf(ms), head = i == 0)
                    }
                }
            } + breaks.map { Tok("\n\n", null, it, false, segOf(it), brk = true) } +
            edits.map { Tok(it.text, null, it.ms, false, segOf(it.ms), edit = it) }
        val bySeg = toks.sortedBy { it.ms }.groupBy { it.seg }
        val doc = mutableListOf(Tok.mark(0))
        doc += bySeg[0].orEmpty()
        inserts.forEachIndexed { i, ins ->
            val last = doc.indexOfLast { !it.mark && it.seg == ins.seg && it.ms <= ins.afterMs }
            val at = if (last >= 0) last + 1 else doc.indexOfFirst { it.mark && it.seg == ins.seg } + 1
            doc.addAll(at, listOf(Tok.mark(i + 1)) + bySeg[i + 1].orEmpty())
        }
        return doc
    }

    /**
     * Start over from [text] (paragraphs a blank line apart), nothing
     * said yet, with the cursor before its [cursor]th word or break, or
     * at the end.
     */
    fun load(text: String, cursor: Int = Int.MAX_VALUE) {
        val items = mutableListOf<String>()
        for (p in text.split(Regex("\\n\\s*\\n"))) {
            val words = p.split(Regex("\\s+")).filter { it.isNotEmpty() }
            if (words.isEmpty()) continue
            if (items.isNotEmpty()) items += BREAK
            items += words
        }
        reset(items, cursor)
    }

    /**
     * Fold everything said and done so far into the text from before,
     * keeping the cursor where it is: for a new session, whose clock
     * starts again at zero. Partial words are dropped.
     */
    fun settle() {
        val doc = build()
        val cursor = cursorAt(doc)
        val items = mutableListOf<String>()
        var at = -1
        var lastBrk = true // nor one first
        doc.forEachIndexed { i, t ->
            if (i == cursor) at = items.size
            if (!shown(t) || t.partial) return@forEachIndexed
            if (t.brk && lastBrk) return@forEachIndexed
            lastBrk = t.brk
            if (t.brk) items += BREAK else items += t.text.split(' ').filter { it.isNotEmpty() }
        }
        reset(items, if (at < 0) items.size else at)
    }

    private fun reset(items: List<String>, cursor: Int) {
        paras = emptyList()
        cuts.clear()
        breaks.clear()
        edits.clear()
        inserts.clear()
        stamped = Long.MIN_VALUE
        // Two ms apart, ending before zero.
        val times = items.indices.map { -2L * (items.size - it) }
        base = items.indices.filter { items[it] != BREAK }.map { items[it] to times[it] }
        items.indices.filter { items[it] == BREAK }.mapTo(breaks) { times[it] }
        // New speech is a cursor move's even at the end, so it's fitted to
        // the sentence it lands in.
        val at = cursor.coerceIn(0, items.size)
        inserts += Insertion(0, if (at > 0) times[at - 1] else Long.MIN_VALUE, 0)
    }

    /** The text as saved: no partial words, paragraphs a blank line apart. */
    fun text(): String =
        lay(build().filter { !it.partial }, "").text.lines().joinToString("\n") { it.trim() }.trim()

    /**
     * [doc] with the seams of cursor moves fixed up: the engine starts
     * every utterance as a sentence, so speech put mid-sentence begins
     * with a capital and may end with a full stop. Where the word before
     * doesn't end a sentence, the first word is lowercased unless it
     * looks like a name: "I", an acronym, inner capitals, or capitalized
     * elsewhere mid-sentence. A full stop before a lowercase word of the
     * old text is dropped. Speech put mid-sentence is a fragment, so the
     * same goes for the utterances within it ("brown" "lazy" said with a
     * pause comes as "Brown." "Lazy.").
     */
    private fun polish(doc: List<Tok>): List<Tok> {
        val out = doc.toMutableList()
        val words = doc.indices.filter { shown(doc[it]) }
        fun ends(t: Tok) = t.brk || t.text.trimEnd('"', '\'', ')', '”', '’').let { it.endsWith('.') || it.endsWith('?') || it.endsWith('!') || it.endsWith('…') }
        fun upper(t: Tok) = t.text.firstOrNull()?.isUpperCase() == true
        // Words the engine capitalized mid-utterance after a lowercase word,
        // so likely names. Not after a capital: an utterance's first word is
        // capitalized as a sentence start and can drag the next one along
        // ("Lazy Brown").
        val names = HashSet<String>()
        for (k in 1 until words.size) {
            val prev = doc[words[k - 1]]
            val t = doc[words[k]]
            if (!t.brk && !t.partial && !t.head && t.seg == prev.seg && !prev.brk &&
                prev.text.firstOrNull()?.isLowerCase() == true && upper(t)) {
                names += t.text.trim { !it.isLetterOrDigit() }
            }
        }
        fun name(w: String): Boolean {
            val core = w.trim { !it.isLetterOrDigit() }
            return core == "I" || core.startsWith("I'") || core.startsWith("I’") ||
                core.drop(1).any { it.isUpperCase() } || core in names
        }
        /** Lowercase the utterance head words[k] and the run of capitals it dragged along. */
        fun lower(k: Int) {
            var m = k
            while (m < words.size) {
                val t = doc[words[m]]
                if (m > k && (t.head || t.brk || t.partial || t.seg != doc[words[k]].seg)) break
                if (t.edit != null) break // typed by hand: as typed
                if (!upper(t) || name(t.text)) break
                out[words[m]] = out[words[m]].withText(out[words[m]].text.replaceFirstChar { it.lowercaseChar() })
                m++
            }
        }
        val fragment = HashSet<Int>()
        for (k in 1 until words.size) {
            val i = words[k - 1]
            val j = words[k]
            val prev = doc[i]
            val t = doc[j]
            if (t.brk || prev.brk || t.partial || prev.partial || t.edit != null || prev.edit != null) continue
            val head = t.text
            if (t.seg == prev.seg) {
                // An utterance within a fragment: carry the sentence on.
                if (!t.head || t.seg !in fragment) continue
                val last = out[i].text
                if (last.endsWith('.') && !last.endsWith("..")) out[i] = out[i].withText(last.dropLast(1))
                if (!ends(out[i])) lower(k)
                continue
            }
            if (!ends(prev)) fragment += t.seg
            if (!ends(prev)) lower(k)
            // out[i], not prev: the first word may have been lowercased already.
            val last = out[i].text
            if (head.firstOrNull()?.isLowerCase() == true && last.endsWith('.') && !last.endsWith("..")) {
                out[i] = out[i].withText(last.dropLast(1))
            }
        }
        return out
    }

    /** Where in [doc] new speech goes: after the latest cursor move's words. */
    fun cursorAt(doc: List<Tok>): Int = doc.indexOfLast { it.seg == inserts.size } + 1

    /**
     * [doc] as text: words a space apart, each shown break once and none
     * first, and [caret] where the cursor is.
     */
    fun lay(doc: List<Tok>, caret: String): Laid {
        val sb = StringBuilder()
        val toks = mutableListOf<Pair<IntRange, Tok>>()
        var caretAt = -1
        val cursor = cursorAt(doc)
        fun caret() {
            if (sb.isNotEmpty() && sb.last() != ' ' && sb.last() != '\n') sb.append(' ')
            caretAt = sb.length
            sb.append(caret)
        }
        var lastBrk = true // nor one first
        doc.forEachIndexed { i, t ->
            if (i == cursor) caret()
            if (!shown(t)) return@forEachIndexed
            // Two breaks can't touch, but a break said meanwhile could
            // land beside one; show it once.
            if (t.brk && lastBrk) return@forEachIndexed
            lastBrk = t.brk
            if (!t.brk && sb.isNotEmpty() && sb.last() != ' ' && sb.last() != '\n' && sb.length != caretAt + caret.length) sb.append(' ')
            if (t.brk && sb.isNotEmpty() && sb.last() == ' ') sb.setLength(sb.length - 1)
            val at = sb.length
            sb.append(t.text)
            toks += (at until sb.length) to t
        }
        if (cursor == doc.size) caret()
        return Laid(sb.toString(), toks, caretAt)
    }

    /**
     * Send new speech to the gap before [tok] (after it if [after]):
     * [now] finishes what's being said and gives the session's time.
     * False if the cursor is there already.
     */
    fun moveCursor(doc: List<Tok>, tok: Tok, after: Boolean, now: () -> Long): Boolean {
        val index = doc.indexOfFirst { it === tok }
        if (index < 0) return false
        val gap = if (after) index + 1 else index
        val cursor = cursorAt(doc)
        // Already there (only hidden words or marks between): nothing moves.
        if ((minOf(gap, cursor) until maxOf(gap, cursor)).none { shown(doc[it]) }) return false
        val from = stamp(now())
        val prev = doc[gap - 1]
        inserts += Insertion(prev.seg, if (prev.mark) Long.MIN_VALUE else prev.ms, from)
        return true
    }

    /**
     * A new paragraph at the cursor, [now] finishing what's being said.
     * Never two in a row, nor one first: false beside a break already there
     * or with nothing before it.
     */
    fun paragraphBreak(doc: List<Tok>, now: () -> Long): Boolean {
        val cursor = cursorAt(doc)
        val before = doc.subList(0, cursor).lastOrNull(::shown)
        val after = doc.subList(cursor, doc.size).firstOrNull(::shown)
        if (before == null || before.brk || after?.brk == true) return false
        breaks += stamp(now())
        return true
    }

    /**
     * Delete [picked]: heard words are cut, retyped ones and breaks
     * removed. Returns the cut words' spans of audio.
     */
    fun delete(picked: List<Tok>): List<LongRange> {
        val spans = mutableListOf<LongRange>()
        for (t in picked) {
            val w = t.time
            when {
                t.brk -> breaks.remove(t.ms)
                t.edit != null -> edits.remove(t.edit)
                w != null -> (w.startMs..maxOf(w.startMs, w.endMs - 1)).let { cuts += it; spans += it }
            }
        }
        // Breaks now side by side become one, so deleting it later
        // doesn't uncover another.
        var lastBrk = true // nor one first
        for (t in document(paras).filter(::shown)) {
            if (t.brk && lastBrk) breaks.remove(t.ms)
            lastBrk = t.brk
        }
        return spans
    }

    /** What retyping [picked] replaces, and its text; null if none of it can be. Partial words and breaks are left as they are. */
    fun editOf(picked: List<Tok>): Retype? {
        val words = picked.filter { !it.brk && !it.mark && !it.partial && (it.time != null || it.edit != null) }
        if (words.isEmpty()) return null
        return Retype(
            cut = words.mapNotNull { t -> t.time?.let { it.startMs..maxOf(it.startMs, it.endMs - 1) } },
            drop = words.mapNotNull { it.edit },
            ms = words.minOf { it.ms },
            endMs = words.maxOf { it.time?.endMs ?: it.edit?.endMs ?: it.ms },
            text = words.joinToString(" ") { it.text },
        )
    }

    /** Replace [r]'s words with [text], placed where the first was said. */
    fun retype(r: Retype, text: String) {
        cuts += r.cut
        edits.removeAll(r.drop)
        val t = text.split(' ').filter { it.isNotEmpty() }.joinToString(" ")
        if (t.isNotEmpty()) edits += Edit(r.ms, t, r.endMs)
    }

}

/** [text] as laid out, where each shown token is in it, and the cursor's offset (-1: none). */
class Laid(val text: String, val toks: List<Pair<IntRange, Tok>>, val caret: Int)

/** The word or break [toks] has nearest [offset], and whether the gap meant is after it rather than before. */
fun nearestGap(toks: List<Pair<IntRange, Tok>>, offset: Int): Pair<Tok, Boolean>? {
    val (at, t) = toks.minByOrNull { (at, _) ->
        when {
            offset < at.first -> at.first - offset
            offset > at.last + 1 -> offset - at.last - 1
            else -> 0
        }
    } ?: return null
    return t to (offset * 2 > at.first + at.last + 1)
}

/**
 * A cursor move: speech from [fromMs] on (session audio) goes after the
 * last word of move [seg]'s speech said by [afterMs], or at its start.
 */
class Insertion(val seg: Int, val afterMs: Long, val fromMs: Long)

/**
 * A word of the transcript, [ms] when it was said (its clip's start if
 * untimed), from cursor move [seg]; a [mark] holds where a move's speech
 * starts and shows nothing; a [brk] is a paragraph break.
 */
class Tok(
    val text: String,
    val time: Word?,
    val ms: Long,
    val partial: Boolean,
    val seg: Int,
    val mark: Boolean = false,
    val brk: Boolean = false,
    /** First word of its clip (utterance). */
    val head: Boolean = false,
    /** Typed by hand, not heard. */
    val edit: Edit? = null,
) {
    fun withText(text: String) = Tok(text, time, ms, partial, seg, mark, brk, head, edit)

    /** Finished text that can be deleted, retyped, or have the cursor put beside it. */
    val placed: Boolean get() = time != null || brk || edit != null

    companion object {
        fun mark(seg: Int) = Tok("", null, Long.MIN_VALUE, false, seg, mark = true)
    }
}

/** Words typed by hand, placed at [ms] on the session's audio clock, for the words said until [endMs]. */
class Edit(val ms: Long, val text: String, val endMs: Long = ms)

/**
 * What an edit replaces: the heard words' [cut] spans and earlier
 * edits to [drop], where it goes ([ms] to [endMs]), and their [text].
 */
class Retype(val cut: List<LongRange>, val drop: List<Edit>, val ms: Long, val endMs: Long, val text: String)

private const val BREAK = "\n\n"

/** A dictation's paragraph as the engine sent it, in the order its clips were said. */
class KPara(val id: String, val clips: List<KClip>)

/**
 * One clip's [words] as shown, with their [times] in the session's audio
 * when known (finished clips); a word without a time can't be deleted or
 * have the cursor put beside it. [startMs] is when the clip began.
 */
class KClip(val partial: Boolean, val startMs: Long, val words: List<String>, val times: List<Word>)

/** An engine `paragraph` object. */
fun parseKPara(o: JSONObject): KPara {
    val clips = o.getJSONArray("clips")
    return KPara(
        id = o.getString("id"),
        clips = (0 until clips.length()).map { parseKClip(clips.getJSONObject(it)) },
    )
}

private fun parseKClip(c: JSONObject): KClip {
    val text = c.getString("text")
    val partial = c.getString("stage") == "partial"
    val words = text.split(Regex("\\s+")).filter { it.isNotEmpty() }
    val start = c.getLong("start_ms")
    if (partial || words.isEmpty()) return KClip(partial, start, words, emptyList())
    val timed = parseWords(c.optJSONArray("words"))
    val times = if (timed.size == words.size) {
        timed
    } else {
        // A pass changed the words without timing them: spread the
        // new ones over the clip, keeping the times of those that match.
        val end = start + c.optLong("duration_ms", 0)
        runCatching { parseWords(Native.alignWords(timed.toJson(), text, start, end)) }
            .getOrDefault(emptyList())
            .takeIf { it.size == words.size }
            .orEmpty()
    }
    return KClip(partial, start, words, times)
}
