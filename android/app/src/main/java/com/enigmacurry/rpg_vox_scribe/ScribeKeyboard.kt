package com.enigmacurry.rpg_vox_scribe

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Typeface
import android.inputmethodservice.InputMethodService
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.ReplacementSpan
import android.util.Log
import android.util.TypedValue
import android.view.ActionMode
import android.view.GestureDetector
import android.view.Gravity
import android.view.Menu
import android.view.MenuItem
import android.view.MotionEvent
import android.view.View
import android.view.WindowInsets
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONArray
import org.json.JSONObject

private const val TAG = "ScribeKeyboard"
private const val RATE = 16_000
private const val DELETE = 1
/** Stands in for the cursor in the transcript's text, drawn by [CaretSpan]. */
private const val CARET = '\u200A'

/**
 * Voice keyboard: pick Scribe as the input method in any app and it
 * starts listening, shows the live transcript (partials dimmed), and
 * Insert types the finished text into the field once passes 2 and 3 are
 * done, then switches back to the previous keyboard. Pause mutes the mic
 * without closing it, so Resume picks up at once. Selecting text offers
 * Delete for its finished words, and a tap moves the purple cursor, where
 * new speech goes, to the nearest gap between words. Both work by time in
 * the session's audio, so they hold when later passes rewrite the text. Uses the models
 * the app loaded, loading them if needed.
 */
class ScribeKeyboard : InputMethodService() {
    private val main = Handler(Looper.getMainLooper())

    private lateinit var status: TextView
    private lateinit var transcript: TextView
    private lateinit var scroll: ScrollView
    private lateinit var insertButton: Button
    private lateinit var pauseButton: Button
    private lateinit var breakButton: Button

    private var session: Dictation? = null
    /** The latest paragraphs, to redraw after a cut. */
    private var paras: List<KPara> = emptyList()
    /** Spans of the session's audio (ms) whose words were deleted. */
    private val cuts = mutableListOf<LongRange>()
    /** Paragraph breaks, each at a time on the session's audio clock (ms). */
    private val breaks = mutableListOf<Long>()
    /** Cursor moves, in order: speech from each one's [Insertion.fromMs] on goes there. */
    private val inserts = mutableListOf<Insertion>()
    /** The transcript as last drawn, in order, hidden words and marks included. */
    private var doc: List<Tok> = emptyList()
    /** Where each finished word is in [transcript]'s text, and its index in [doc]. */
    private var placed: List<Pair<IntRange, Int>> = emptyList()
    /** A selection is open: updates wait, so it isn't wiped by a redraw. */
    private var selecting = false
    /** Keep the cursor in view: it was in view when the transcript last changed. */
    private var follow = true
    /** Where the cursor is in [transcript]'s text. */
    private var caretAt = 0
    /** Waiting for the models to load before starting. */
    private var waiting = false
    private var dark = false

    override fun onCreate() {
        super.onCreate()
        Scribe.init(this)
    }

    override fun onCreateInputView(): View {
        dark = resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        val fg = if (dark) Color.WHITE else Color.BLACK
        val dp = { v: Int -> TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics).toInt() }
        status = TextView(this).apply {
            setTextColor(if (dark) 0xFFAAAAAA.toInt() else 0xFF555555.toInt())
            textSize = 13f
        }
        transcript = TextView(this).apply {
            setTextColor(fg)
            textSize = 18f
            setTextIsSelectable(true)
            customSelectionActionModeCallback = DeleteAction()
        }
        val taps = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onSingleTapUp(e: MotionEvent): Boolean {
                moveCursor(transcript.getOffsetForPosition(e.x, e.y))
                return false
            }
        })
        // Watch for taps but leave the touch to the text, for long-press selection.
        transcript.setOnTouchListener { _, e ->
            taps.onTouchEvent(e)
            false
        }
        scroll = object : ScrollView(this) {
            // The selectable text's cursor sits at its start; don't let
            // it drag the view back to the top on every update.
            override fun requestChildRectangleOnScreen(child: View, rect: android.graphics.Rect, immediate: Boolean) = false
        }.apply {
            addView(transcript, MATCH_PARENT, WRAP_CONTENT)
        }
        // Scroll again once grown text is laid out: until then the scroll
        // view can't go past the old end.
        transcript.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ ->
            if (follow) scroll.post { revealCaret() }
        }
        val keyboards = Button(this).apply {
            text = "⌨"
            setOnClickListener { leave() }
        }
        val cancel = Button(this).apply {
            text = "Cancel"
            setOnClickListener {
                session?.cancel()
                session = null
                leave()
            }
        }
        pauseButton = Button(this).apply {
            text = "Pause"
            setOnClickListener { togglePause() }
        }
        breakButton = Button(this).apply {
            text = "¶"
            setOnClickListener { paragraphBreak() }
        }
        insertButton = Button(this).apply {
            text = "Insert"
            setTypeface(typeface, Typeface.BOLD)
            setOnClickListener { insert() }
        }
        val buttons = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.END
            addView(keyboards, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))
            addView(View(context), LinearLayout.LayoutParams(0, 0, 1f))
            addView(cancel)
            addView(breakButton, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))
            addView(pauseButton)
            addView(insertButton)
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(if (dark) 0xFF1E1F22.toInt() else 0xFFF1F3F4.toInt())
            setPadding(dp(16), dp(10), dp(16), dp(6))
            // The IME window is edge to edge: keep the buttons clear of the
            // navigation bar's back and keyboard-switch buttons.
            setOnApplyWindowInsetsListener { v, insets ->
                val (l, r, bottom) = if (Build.VERSION.SDK_INT >= 30) {
                    insets.getInsets(WindowInsets.Type.navigationBars()).let { Triple(it.left, it.right, it.bottom) }
                } else {
                    @Suppress("DEPRECATION")
                    Triple(insets.systemWindowInsetLeft, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
                }
                v.setPadding(dp(16) + l, dp(10), dp(16) + r, dp(6) + bottom)
                insets
            }
            addView(status)
            addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, dp(170)))
            addView(buttons, MATCH_PARENT, WRAP_CONTENT)
        }
    }

    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        val s = session
        when {
            s != null -> {
                // Back from another app or field: it kept going meanwhile.
                insertButton.isEnabled = true
                pauseButton.isEnabled = true
                breakButton.isEnabled = true
                pauseButton.text = if (s.paused) "Resume" else "Pause"
                status.text = if (s.paused) "Paused" else "Listening…"
                render(paras)
            }
            !waiting -> begin()
        }
    }

    override fun onFinishInputView(finishingInput: Boolean) {
        // Hidden: keep listening (unless paused), so you can switch apps
        // to read something out; Insert or Cancel ends it.
        waiting = false
        super.onFinishInputView(finishingInput)
    }

    override fun onDestroy() {
        session?.cancel()
        session = null
        super.onDestroy()
    }

    private fun begin() {
        transcript.text = ""
        paras = emptyList()
        cuts.clear()
        breaks.clear()
        inserts.clear()
        insertButton.isEnabled = false
        pauseButton.isEnabled = false
        breakButton.isEnabled = false
        pauseButton.text = "Pause"
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            status.text = "Open Scribe once and allow the microphone."
            return
        }
        when (Scribe.phase) {
            is Phase.NeedModels, is Phase.Installing -> {
                status.text = "Open Scribe to download the speech models."
                return
            }
            is Phase.Failed -> {
                status.text = "The speech models failed to load; open Scribe."
                return
            }
            else -> {}
        }
        if (Scribe.busy()) {
            status.text = "Scribe is recording; stop it to use voice typing."
            return
        }
        if (!Scribe.loaded()) {
            status.text = "Loading speech models…"
            waiting = true
            Scribe.load()
            awaitModels()
            return
        }
        start()
    }

    private fun awaitModels() {
        main.postDelayed({
            if (!waiting) return@postDelayed
            when {
                Scribe.loaded() -> {
                    waiting = false
                    start()
                }
                Scribe.phase is Phase.Failed -> {
                    waiting = false
                    status.text = "The speech models failed to load; open Scribe."
                }
                else -> awaitModels()
            }
        }, 200)
    }

    private fun start() {
        val models = Scribe.claimForKeyboard()
        if (models == 0L) {
            status.text = "Scribe is busy."
            return
        }
        status.text = "Listening…"
        insertButton.isEnabled = true
        pauseButton.isEnabled = true
        breakButton.isEnabled = true
        pauseButton.text = "Pause"
        session = Dictation(
            models,
            getSystemService(AudioManager::class.java),
            Scribe.boost,
            Scribe.route,
            main,
            onChange = ::render,
        ).also { it.start() }
    }

    private fun isCut(w: Word) = cuts.any { w.startMs in it }

    private fun shown(t: Tok) = !t.mark && (t.time == null || !isCut(t.time))

    /** The cursor move whose speech [ms] is, 0 for none (the original run). */
    private fun segOf(ms: Long): Int {
        var seg = 0
        inserts.forEachIndexed { i, ins -> if (ins.fromMs <= ms) seg = i + 1 }
        return seg
    }

    /**
     * All the words in reading order, as one running paragraph (the
     * engine's breaks are pauses, not intent). Each cursor move's speech,
     * in the order said, goes after the word it was placed after, found
     * by time so later passes rewriting the text don't move it.
     */
    private fun document(paras: List<KPara>): List<Tok> {
        val toks = paras.flatMap { p ->
            p.clips.flatMap { c ->
                c.words.mapIndexed { i, w ->
                    val t = c.times.getOrNull(i)
                    val ms = t?.startMs ?: c.startMs
                    Tok(w, t, ms, c.partial, segOf(ms))
                }
            }
        } + breaks.map { Tok("\n\n", null, it, false, segOf(it), brk = true) }
        // sortedBy is stable: words of one clip keep their order.
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
     * [doc] with the seams of cursor moves fixed up: the engine starts
     * every utterance as a sentence, so speech put mid-sentence begins
     * with a capital and may end with a full stop. Where the word before
     * doesn't end a sentence, the first word is lowercased unless it
     * looks like a name: "I", an acronym, inner capitals, or capitalized
     * elsewhere mid-sentence. A full stop before a lowercase word of the
     * old text is dropped.
     */
    private fun polish(doc: List<Tok>): List<Tok> {
        val out = doc.toMutableList()
        val words = doc.indices.filter { shown(doc[it]) }
        fun ends(t: Tok) = t.brk || t.text.trimEnd('"', '\'', ')', '”', '’').let { it.endsWith('.') || it.endsWith('?') || it.endsWith('!') || it.endsWith('…') }
        // Words the engine capitalized mid-sentence, so likely names.
        val names = HashSet<String>()
        for (k in 1 until words.size) {
            val prev = doc[words[k - 1]]
            val t = doc[words[k]]
            if (!t.brk && !t.partial && t.seg == prev.seg && !ends(prev) && t.text.firstOrNull()?.isUpperCase() == true) {
                names += t.text.trim { !it.isLetterOrDigit() }
            }
        }
        fun name(w: String): Boolean {
            val core = w.trim { !it.isLetterOrDigit() }
            return core == "I" || core.startsWith("I'") || core.startsWith("I’") ||
                core.drop(1).any { it.isUpperCase() } || core in names
        }
        for (k in 1 until words.size) {
            val i = words[k - 1]
            val j = words[k]
            val prev = doc[i]
            val t = doc[j]
            if (t.seg == prev.seg || t.brk || prev.brk || t.partial || prev.partial) continue
            val head = t.text
            if (!ends(prev) && head.firstOrNull()?.isUpperCase() == true && !name(head)) {
                out[j] = t.withText(head.replaceFirstChar { it.lowercaseChar() })
            }
            if (head.firstOrNull()?.isLowerCase() == true && prev.text.endsWith('.') && !prev.text.endsWith("..")) {
                out[i] = out[i].withText(prev.text.dropLast(1))
            }
        }
        return out
    }

    /** Where in [doc] new speech goes: after the latest cursor move's words. */
    private fun cursorAt(doc: List<Tok>): Int = doc.indexOfLast { it.seg == inserts.size } + 1

    private fun render(paras: List<KPara>) {
        this.paras = paras
        if (selecting) return
        // Follow the cursor only while it's in view, so scrolling away
        // to reread earlier text isn't undone by the next update.
        follow = caretLines()?.let { (top, bottom) -> top >= scroll.scrollY && bottom <= scroll.scrollY + scroll.height } ?: true
        val dim = 0xFF888888.toInt()
        val sb = SpannableStringBuilder()
        val placed = mutableListOf<Pair<IntRange, Int>>()
        val doc = polish(document(paras))
        val cursor = cursorAt(doc)
        fun caret() {
            if (sb.isNotEmpty() && sb.last() != ' ' && sb.last() != '\n') sb.append(' ')
            caretAt = sb.length
            sb.append(CARET)
            val purple = if (dark) 0xFFCE93D8.toInt() else 0xFF8E24AA.toInt()
            sb.setSpan(CaretSpan(purple, resources.displayMetrics.density * 2), sb.length - 1, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
        var lastBrk = false
        doc.forEachIndexed { i, t ->
            if (i == cursor) caret()
            if (!shown(t)) return@forEachIndexed
            // Two breaks can't touch, but a break said meanwhile could
            // land beside one; show it once.
            if (t.brk && lastBrk) return@forEachIndexed
            lastBrk = t.brk
            if (!t.brk && sb.isNotEmpty() && sb.last() != ' ' && sb.last() != '\n' && sb.last() != CARET) sb.append(' ')
            if (t.brk && sb.isNotEmpty() && sb.last() == ' ') sb.delete(sb.length - 1, sb.length)
            val at = sb.length
            sb.append(t.text)
            when {
                t.partial -> sb.setSpan(ForegroundColorSpan(dim), at, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                t.time != null || t.brk -> placed += (at until sb.length) to i
            }
        }
        if (cursor == doc.size) caret()
        transcript.text = sb
        this.doc = doc
        this.placed = placed
        if (follow) scroll.post { revealCaret() }
    }

    /** Top and bottom of the cursor's line in [transcript], once laid out. */
    private fun caretLines(): Pair<Int, Int>? {
        val l = transcript.layout ?: return null
        if (caretAt > transcript.text.length) return null
        val line = l.getLineForOffset(caretAt)
        return (transcript.paddingTop + l.getLineTop(line)) to (transcript.paddingTop + l.getLineBottom(line))
    }

    /** Scroll just enough to show the cursor's line. */
    private fun revealCaret() {
        val (top, bottom) = caretLines() ?: return
        when {
            bottom > scroll.scrollY + scroll.height -> scroll.scrollTo(0, bottom - scroll.height)
            top < scroll.scrollY -> scroll.scrollTo(0, top)
        }
    }

    /**
     * Tapped at [offset] in the text: finish what's being said, then send
     * new speech to the gap between words nearest the tap.
     */
    private fun moveCursor(offset: Int) {
        val s = session ?: return
        if (selecting) return
        val near = placed.minByOrNull { (at, _) ->
            when {
                offset < at.first -> at.first - offset
                offset > at.last + 1 -> offset - at.last - 1
                else -> 0
            }
        } ?: return
        val (at, index) = near
        // The gap is before doc[gap]: after the word when the tap is past its middle.
        val gap = if (offset * 2 > at.first + at.last + 1) index + 1 else index
        val from = s.breakNow()
        val cursor = cursorAt(doc)
        // Already there (only hidden words or marks between): nothing moves.
        if ((minOf(gap, cursor) until maxOf(gap, cursor)).none { shown(doc[it]) }) return
        val prev = doc[gap - 1]
        inserts += if (prev.mark) {
            Insertion(prev.seg, Long.MIN_VALUE, from)
        } else {
            Insertion(prev.seg, prev.ms, from)
        }
        render(paras)
    }

    /** Adds Delete to the selection's menu: it cuts every finished word the selection touches. */
    private inner class DeleteAction : ActionMode.Callback {
        override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
            selecting = true
            menu.add(Menu.NONE, DELETE, 0, "Delete")
            return true
        }

        override fun onPrepareActionMode(mode: ActionMode, menu: Menu) = false

        override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
            if (item.itemId != DELETE) return false
            val a = minOf(transcript.selectionStart, transcript.selectionEnd)
            val b = maxOf(transcript.selectionStart, transcript.selectionEnd)
            for ((at, i) in placed) {
                if (at.first >= b || at.last < a) continue
                val t = doc[i]
                val w = t.time
                when {
                    t.brk -> breaks.remove(t.ms)
                    w != null -> cuts += w.startMs..maxOf(w.startMs, w.endMs - 1)
                }
            }
            // Breaks now side by side become one, so deleting it later
            // doesn't uncover another.
            var lastBrk = false
            for (t in document(paras).filter(::shown)) {
                if (t.brk && lastBrk) breaks.remove(t.ms)
                lastBrk = t.brk
            }
            mode.finish()
            return true
        }

        override fun onDestroyActionMode(mode: ActionMode) {
            selecting = false
            render(paras)
        }
    }

    /** Finish what's being said and start a new paragraph at the cursor. */
    private fun paragraphBreak() {
        val s = session ?: return
        val cursor = cursorAt(doc)
        // Never two in a row: not beside a break already there.
        val before = doc.subList(0, cursor).lastOrNull(::shown)
        val after = doc.subList(cursor, doc.size).firstOrNull(::shown)
        if (before?.brk == true || after?.brk == true) return
        breaks += s.breakNow()
        render(paras)
    }

    private fun togglePause() {
        val s = session ?: return
        s.paused = !s.paused
        pauseButton.text = if (s.paused) "Resume" else "Pause"
        status.text = if (s.paused) "Paused" else "Listening…"
    }

    private fun insert() {
        val s = session ?: return
        session = null
        insertButton.isEnabled = false
        pauseButton.isEnabled = false
        breakButton.isEnabled = false
        status.text = "Finishing…"
        s.finish { paras ->
            val ic = currentInputConnection
            val multiLine = (currentInputEditorInfo?.inputType ?: 0) and EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE != 0
            val sb = StringBuilder()
            var lastBrk = false
            for (t in polish(document(paras)).filter(::shown)) {
                if (t.brk && lastBrk) continue
                lastBrk = t.brk
                if (t.brk) {
                    // A one-line field gets a space instead.
                    while (sb.endsWith(' ')) sb.setLength(sb.length - 1)
                    sb.append(if (multiLine) "\n\n" else " ")
                } else {
                    if (sb.isNotEmpty() && !sb.last().isWhitespace()) sb.append(' ')
                    sb.append(t.text)
                }
            }
            var text = sb.toString()
            if (ic != null && text.isNotEmpty()) {
                val before = ic.getTextBeforeCursor(1, 0)
                if (!before.isNullOrEmpty() && !before.last().isWhitespace() && !text.first().isWhitespace()) text = " $text"
                ic.commitText(text, 1)
            }
            leave()
        }
    }

    /** Back to the keyboard the user came from. */
    private fun leave() {
        if (Build.VERSION.SDK_INT >= 28 && switchToPreviousInputMethod()) return
        requestHideSelf(0)
    }
}

/**
 * A cursor move: speech from [fromMs] on (session audio) goes after the
 * last word of move [seg]'s speech said by [afterMs], or at its start.
 */
private class Insertion(val seg: Int, val afterMs: Long, val fromMs: Long)

/**
 * A word of the transcript, [ms] when it was said (its clip's start if
 * untimed), from cursor move [seg]; a [mark] holds where a move's speech
 * starts and shows nothing; a [brk] is a paragraph break.
 */
private class Tok(
    val text: String,
    val time: Word?,
    val ms: Long,
    val partial: Boolean,
    val seg: Int,
    val mark: Boolean = false,
    /** A paragraph break the ¶ button put in. */
    val brk: Boolean = false,
) {
    fun withText(text: String) = Tok(text, time, ms, partial, seg, mark, brk)

    companion object {
        fun mark(seg: Int) = Tok("", null, Long.MIN_VALUE, false, seg, mark = true)
    }
}

/** The cursor: a full-height bar [width] px wide. */
private class CaretSpan(color: Int, private val width: Float) : ReplacementSpan() {
    private val paint = Paint().apply { this.color = color }

    override fun getSize(paint: Paint, text: CharSequence?, start: Int, end: Int, fm: Paint.FontMetricsInt?): Int {
        fm?.let { paint.getFontMetricsInt(it) }
        return width.toInt() + 1
    }

    override fun draw(canvas: Canvas, text: CharSequence?, start: Int, end: Int, x: Float, top: Int, y: Int, bottom: Int, paint: Paint) {
        canvas.drawRect(x, top.toFloat(), x + width, bottom.toFloat(), this.paint)
    }
}

/**
 * One keyboard dictation: an engine session fed by the mic. Events
 * arrive on [main] through [onChange]; [finish] or [cancel] ends it and
 * frees the session.
 */
private class Dictation(
    private val models: Long,
    private val am: AudioManager,
    private val boost: Boost,
    private val route: MicRoute,
    private val main: Handler,
    private val onChange: (List<KPara>) -> Unit,
) {
    private var session = 0L
    @Volatile private var capturing = true
    @Volatile private var polling = true
    @Volatile private var ended = false
    /** The mic keeps running but its audio is dropped, so resuming is instant. */
    @Volatile var paused = false
    /** Samples pushed so far: the session's audio clock. */
    @Volatile private var pushed = 0L
    @Volatile private var breakAsked = false
    private var capture: Thread? = null
    private var poller: Thread? = null
    private val paras = LinkedHashMap<String, KPara>()

    fun start() {
        session = Native.start(models, RATE, true, null)
        poller = Thread({
            while (polling) {
                val events = Native.poll(session, 100)
                if (events.length > 2) main.post { apply(events) }
            }
        }, "ime-poll").apply { start() }
        capture = Thread(::record, "ime-mic").apply { start() }
    }

    /** Close the utterance in progress; returns now on the session's clock (ms). */
    fun breakNow(): Long {
        breakAsked = true
        return pushed * 1000 / RATE
    }

    @SuppressLint("MissingPermission")
    private fun record() {
        val routing = MicRouting(am)
        val gain = MicGain(boost)
        val rec = runCatching {
            val min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_RECOGNITION,
                RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(min, RATE),
            ).also { r -> if (route == MicRoute.AUTO) routing.headset()?.let { r.preferredDevice = it } }
        }.getOrNull()
        if (rec == null || rec.state != AudioRecord.STATE_INITIALIZED) {
            Log.e(TAG, "AudioRecord failed to initialize")
            rec?.release()
            routing.release()
            return
        }
        val shorts = ShortArray(RATE / 10)
        val buf = FloatArray(shorts.size)
        rec.startRecording()
        Log.i(TAG, "mic: ${rec.inputName()}")
        var wasPaused = false
        try {
            while (capturing) {
                val n = rec.read(shorts, 0, shorts.size, AudioRecord.READ_BLOCKING)
                if (n <= 0) continue
                if (breakAsked) {
                    breakAsked = false
                    Native.breakParagraph(session)
                }
                val p = paused
                if (p != wasPaused) {
                    wasPaused = p
                    // Close the utterance the pause cut off. Here rather
                    // than on the main thread so it can't race a push.
                    if (p) Native.breakParagraph(session)
                }
                if (p) continue
                for (i in 0 until n) buf[i] = shorts[i] / 32768f
                gain.process(buf, n)
                Native.push(session, buf, n)
                pushed += n
            }
        } finally {
            rec.stop()
            rec.release()
            routing.release()
        }
    }

    private fun apply(json: String) {
        if (ended) return
        val events = JSONArray(json)
        var changed = false
        for (i in 0 until events.length()) {
            val ev = events.getJSONObject(i)
            when (ev.getString("t")) {
                "paragraph" -> parse(ev.getJSONObject("p")).let { paras[it.id] = it; changed = true }
                "removed" -> changed = paras.remove(ev.getString("id")) != null || changed
            }
        }
        // Level events arrive every 50 ms; only text changes redraw.
        if (changed) onChange(paras.values.toList())
    }

    /** Stop listening, let the passes finish, then hand back the final paragraphs on [main]. */
    fun finish(done: (List<KPara>) -> Unit) = end { json ->
        val list = JSONObject(json).optJSONArray("paragraphs") ?: JSONArray()
        val final = (0 until list.length()).map { parse(list.getJSONObject(it)) }
        main.post { done(final) }
    }

    fun cancel() = end {}

    private fun end(then: (String) -> Unit) {
        if (ended) return
        ended = true
        capturing = false
        Thread({
            // No push may reach the session once it is finished.
            capture?.join()
            val json = Native.finish(session)
            polling = false
            poller?.join()
            Native.free(session)
            main.post { Scribe.releaseFromKeyboard() }
            then(json)
        }, "ime-finish").start()
    }

    private fun parse(o: JSONObject): KPara {
        val clips = o.getJSONArray("clips")
        return KPara(
            id = o.getString("id"),
            clips = (0 until clips.length()).map { clip(clips.getJSONObject(it)) },
        )
    }

    private fun clip(c: JSONObject): KClip {
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
}

/** A keyboard paragraph, in the order its clips were said. */
private class KPara(val id: String, val clips: List<KClip>)

/**
 * One clip's [words] as shown, with their [times] in the session's audio
 * when known (finished clips); a word without a time can't be deleted or
 * have the cursor put beside it. [startMs] is when the clip began.
 */
private class KClip(val partial: Boolean, val startMs: Long, val words: List<String>, val times: List<Word>)
