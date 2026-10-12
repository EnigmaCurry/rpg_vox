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
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONArray
import org.json.JSONObject

private const val TAG = "ScribeKeyboard"
private const val RATE = 16_000
private const val DELETE = 1
private const val EDIT = 2
/**
 * Stands in for the cursor in the transcript's text, drawn by [CaretSpan].
 * Not a space: a space ending a line isn't drawn, so the cursor vanished
 * there. This one wraps with the word after it.
 */
private const val CARET = '|'

/**
 * Voice keyboard: pick Scribe as the input method in any app and it
 * starts listening, shows the live transcript (partials dimmed), and
 * Done types the finished text into the field once passes 2 and 3 are
 * done, then switches back to the previous keyboard. Pause mutes the mic
 * without closing it, so Resume picks up at once. Selecting text offers
 * Delete for its finished words, and a tap moves the purple cursor, where
 * new speech goes, to the nearest gap between words. Edit, also on the
 * selection's menu, pauses and retypes the words on a small built-in
 * keyboard. All of it works by time in the session's audio, so it holds
 * when later passes rewrite the text. Uses the models
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
    /** The transcript and buttons, hidden behind [loading] while the models load. */
    private lateinit var content: LinearLayout
    private lateinit var loading: TextView
    /** Edit mode: the line being typed and Scribe's own keys, over [content]. */
    private lateinit var editPanel: LinearLayout
    private lateinit var editLine: TextView
    private val letterKeys = mutableListOf<Button>()

    private var session: Dictation? = null
    /** The text said, with the cursor moves, cuts, edits and breaks made to it. */
    private var draft = DictationDoc()
    /** The latest paragraphs, to redraw after a cut. */
    private var paras: List<KPara> = emptyList()
    /** The open edit, or null outside edit mode. */
    private var editing: PendingEdit? = null
    /** The transcript as last drawn, in order, hidden words and marks included. */
    private var doc: List<Tok> = emptyList()
    /** Where each finished word or break is in [transcript]'s text. */
    private var placed: List<Pair<IntRange, Tok>> = emptyList()
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
        // Back to the other keyboard, dropping what was said: Cancel and
        // switch keyboards in one.
        val keyboards = Button(this).apply {
            text = "⌨"
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
            text = "Done"
            setTypeface(typeface, Typeface.BOLD)
            setOnClickListener { insert() }
        }
        val buttons = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            // All the same width, sharing the row, so each is easy to hit.
            for (b in listOf(keyboards, breakButton, pauseButton, insertButton)) {
                b.minWidth = 0
                b.minimumWidth = 0
                b.setPadding(0, b.paddingTop, 0, b.paddingBottom)
                addView(b, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginEnd = dp(4) })
            }
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(if (dark) 0xFF1E1F22.toInt() else 0xFFF1F3F4.toInt())
            setPadding(dp(16), dp(10), dp(16), dp(14))
            // The IME window is edge to edge: keep the buttons clear of the
            // navigation bar's back and keyboard-switch buttons.
            setOnApplyWindowInsetsListener { v, insets ->
                val (l, r, bottom) = if (Build.VERSION.SDK_INT >= 30) {
                    insets.getInsets(WindowInsets.Type.navigationBars()).let { Triple(it.left, it.right, it.bottom) }
                } else {
                    @Suppress("DEPRECATION")
                    Triple(insets.systemWindowInsetLeft, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
                }
                v.setPadding(dp(16) + l, dp(10), dp(16) + r, dp(14) + bottom)
                insets
            }
            content = LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                addView(status)
                addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, dp(170)))
                addView(buttons, MATCH_PARENT, WRAP_CONTENT)
            }
            loading = TextView(context).apply {
                text = "Loading models…"
                setTextColor(fg)
                textSize = 24f
                gravity = Gravity.CENTER
                visibility = View.GONE
            }
            editPanel = buildEditPanel(fg, dp)
            // Stacked, so the keyboard keeps its height while loading or editing.
            addView(FrameLayout(context).apply {
                addView(content, MATCH_PARENT, WRAP_CONTENT)
                addView(loading, MATCH_PARENT, MATCH_PARENT)
                addView(editPanel, MATCH_PARENT, MATCH_PARENT)
            }, MATCH_PARENT, WRAP_CONTENT)
        }
    }

    /** The edit line over four rows of keys: a plain layout, no prediction. */
    private fun buildEditPanel(fg: Int, dp: (Int) -> Int): LinearLayout {
        editLine = TextView(this).apply {
            setTextColor(fg)
            textSize = 20f
            setSingleLine()
            setPadding(dp(4), dp(6), dp(4), dp(6))
            setOnTouchListener { v, e ->
                if (e.action == MotionEvent.ACTION_UP) {
                    val ed = editing
                    if (ed != null) {
                        var at = (v as TextView).getOffsetForPosition(e.x, e.y)
                        if (at > ed.cursor) at-- // past the caret's own character
                        ed.cursor = at.coerceIn(0, ed.text.length)
                        drawEdit()
                    }
                }
                true
            }
        }
        fun key(label: String, weight: Float = 1f, onTap: () -> Unit) = Button(this).apply {
            text = label
            isAllCaps = false
            textSize = 17f
            minWidth = 0
            minimumWidth = 0
            minHeight = 0
            minimumHeight = 0
            setPadding(0, 0, 0, 0)
            setOnClickListener { onTap() }
            layoutParams = LinearLayout.LayoutParams(0, MATCH_PARENT, weight).apply { marginEnd = dp(3) }
        }
        fun row(vararg keys: Button) = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            keys.forEach { addView(it) }
        }
        fun letters(chars: String) = chars.map { c ->
            key(c.toString()) { typeKey(if (c.isLetter() && editing?.shift == true) c.uppercaseChar() else c) }
                .also { if (c.isLetter()) letterKeys += it }
        }.toTypedArray()
        letterKeys.clear()
        val rows = listOf(
            row(*letters("qwertyuiop")),
            row(*letters("asdfghjkl'")),
            row(key("⇧") { editing?.let { it.shift = !it.shift; drawEdit() } }, *letters("zxcvbnm-"), key("⌫") { backspace() }),
            row(
                key("Cancel", 2f) { closeEdit(apply = false) },
                key(",") { typeKey(',') },
                key("space", 4f) { typeKey(' ') },
                key(".") { typeKey('.') },
                key("OK", 2f) { closeEdit(apply = true) }.apply { setTypeface(typeface, Typeface.BOLD) },
            ),
        )
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            visibility = View.GONE
            setBackgroundColor(if (dark) 0xFF1E1F22.toInt() else 0xFFF1F3F4.toInt())
            addView(editLine, MATCH_PARENT, WRAP_CONTENT)
            rows.forEach { addView(it, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f).apply { bottomMargin = dp(3) }) }
        }
    }

    /**
     * Edit the selection's words: pause, and retype them on the built-in
     * keys. Partial words and breaks in the selection are left as they are.
     */
    private fun openEdit(a: Int, b: Int) {
        val s = session ?: return
        val retype = draft.editOf(picked(a, b)) ?: return
        editing = PendingEdit(
            retype,
            wasPaused = s.paused,
            text = StringBuilder(retype.text),
            cursor = retype.text.length,
        )
        s.paused = true
        editPanel.visibility = View.VISIBLE
        content.visibility = View.INVISIBLE
        drawEdit()
    }

    /** Leave edit mode, replacing the words with what was typed if [apply]; listening resumes unless it was paused before. */
    private fun closeEdit(apply: Boolean) {
        val ed = editing ?: return
        editing = null
        editPanel.visibility = View.GONE
        content.visibility = View.VISIBLE
        if (apply) draft.retype(ed.retype, ed.text.toString())
        session?.let {
            it.paused = ed.wasPaused
            pauseButton.text = if (it.paused) "Resume" else "Pause"
            status.text = if (it.paused) "Paused" else "Listening…"
        }
        render(paras)
    }

    private fun typeKey(c: Char) {
        val ed = editing ?: return
        ed.text.insert(ed.cursor, c)
        ed.cursor++
        ed.shift = false
        drawEdit()
    }

    private fun backspace() {
        val ed = editing ?: return
        if (ed.cursor == 0) return
        ed.text.deleteCharAt(ed.cursor - 1)
        ed.cursor--
        drawEdit()
    }

    private fun drawEdit() {
        val ed = editing ?: return
        val sb = SpannableStringBuilder(ed.text)
        sb.insert(ed.cursor, CARET.toString())
        val purple = if (dark) 0xFFCE93D8.toInt() else 0xFF8E24AA.toInt()
        sb.setSpan(CaretSpan(purple, resources.displayMetrics.density * 2), ed.cursor, ed.cursor + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        editLine.text = sb
        for (k in letterKeys) k.text = k.text.toString().let { if (ed.shift) it.uppercase() else it.lowercase() }
    }

    private fun showLoading(on: Boolean) {
        loading.visibility = if (on) View.VISIBLE else View.GONE
        content.visibility = if (on) View.INVISIBLE else View.VISIBLE
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
        // to read something out; Done or ⌨ ends it.
        waiting = false
        showLoading(false)
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
        if (editing != null) closeEdit(apply = false)
        draft = DictationDoc()
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
            status.text = ""
            showLoading(true)
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
                    showLoading(false)
                    start()
                }
                Scribe.phase is Phase.Failed -> {
                    waiting = false
                    showLoading(false)
                    status.text = "The speech models failed to load; open Scribe."
                }
                else -> awaitModels()
            }
        }, 200)
    }

    private fun start() {
        val models = Scribe.claimForDictation()
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

    /** The finished words and breaks the selection [a] until [b] touches. */
    private fun picked(a: Int, b: Int): List<Tok> =
        placed.filter { (at, _) -> at.first < b && at.last >= a }.map { it.second }

    private fun render(paras: List<KPara>) {
        this.paras = paras
        draft.paras = paras
        if (selecting) return
        // Follow the cursor only while it's in view, so scrolling away
        // to reread earlier text isn't undone by the next update.
        follow = caretLines()?.let { (top, bottom) -> top >= scroll.scrollY && bottom <= scroll.scrollY + scroll.height } ?: true
        val doc = draft.build()
        val laid = draft.lay(doc, CARET.toString())
        val sb = SpannableStringBuilder(laid.text)
        for ((at, t) in laid.toks) {
            if (t.partial) sb.setSpan(ForegroundColorSpan(0xFF888888.toInt()), at.first, at.last + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
        caretAt = laid.caret
        val purple = if (dark) 0xFFCE93D8.toInt() else 0xFF8E24AA.toInt()
        sb.setSpan(CaretSpan(purple, resources.displayMetrics.density * 2), caretAt, caretAt + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        transcript.text = sb
        this.doc = doc
        this.placed = laid.toks.filter { it.second.placed }
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
        val (tok, after) = nearestGap(placed, offset) ?: return
        if (draft.moveCursor(doc, tok, after) { s.breakNow() }) render(paras)
    }

    /**
     * Adds Delete and Edit to the selection's menu. Delete cuts every
     * finished word the selection touches; Edit retypes them.
     */
    private inner class DeleteAction : ActionMode.Callback {
        override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
            selecting = true
            menu.add(Menu.NONE, DELETE, 0, "Delete")
            menu.add(Menu.NONE, EDIT, 1, "Edit")
            return true
        }

        override fun onPrepareActionMode(mode: ActionMode, menu: Menu) = false

        override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
            val a = minOf(transcript.selectionStart, transcript.selectionEnd)
            val b = maxOf(transcript.selectionStart, transcript.selectionEnd)
            if (item.itemId == EDIT) {
                // Before finish(): that redraws with any held-back updates,
                // and a and b are offsets in the text as it is now.
                openEdit(a, b)
                mode.finish()
                return true
            }
            if (item.itemId != DELETE) return false
            draft.delete(picked(a, b))
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
        if (draft.paragraphBreak(doc) { s.breakNow() }) render(paras)
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
            var lastBrk = true // nor one first
            for (t in draft.build(paras).filter(draft::shown)) {
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

/** An open edit: what it replaces, and the line being typed. */
private class PendingEdit(
    val retype: Retype,
    val wasPaused: Boolean,
    val text: StringBuilder,
    var cursor: Int,
    var shift: Boolean = false,
)

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
