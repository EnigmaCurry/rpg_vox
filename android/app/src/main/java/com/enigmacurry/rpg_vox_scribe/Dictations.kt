package com.enigmacurry.rpg_vox_scribe

import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue

/**
 * Dictations: text written by voice and edited in place, as in the voice
 * keyboard, but saved, so you can come back and keep going. Unlike a
 * thread's transcript, which runs in the order things were said, a tap
 * sends new speech anywhere in the text. No audio is kept.
 *
 * The open one's text is a [DictationDoc]; a [Dictation] adds to it while
 * listening. Each session folds into the text when it ends, which is
 * saved then and a moment after each change. In [typing] mode the text
 * is a plain field for the phone's keyboard instead, the cursor carried
 * across both ways. Main thread only.
 */
object Dictations {
    private val main = Handler(Looper.getMainLooper())
    private val store get() = Scribe.threads
    private lateinit var audio: AudioManager

    /** Every dictation, the most recently changed first. */
    val list = mutableStateListOf<DictationInfo>()
    /** The dictation on screen, null for the threads. */
    var open by mutableStateOf<DictationInfo?>(null)
        private set
    /** Its text as shown, the cursor standing on [Laid.caret]. */
    var laid by mutableStateOf(Laid("", emptyList(), -1))
        private set
    var listening by mutableStateOf(false)
        private set
    /** The mic's level while listening (RMS). */
    var level by mutableFloatStateOf(0f)
        private set
    /** Stopped, the last words still being transcribed. */
    var finishing by mutableStateOf(false)
        private set
    /** Typing mode: the text as an ordinary field for the keyboard, null when dictating. */
    var typing by mutableStateOf<TextFieldValue?>(null)
        private set

    private val doc = DictationDoc()
    /** [doc] as last built. */
    private var built: List<Tok> = emptyList()
    private var session: Dictation? = null
    /** Listening when typing began, to take up again after. */
    private var listenAfterTyping = false
    private val saver = Runnable { save() }
    /** Runs once the session finishing has saved. */
    private var then: (() -> Unit)? = null

    fun init(context: android.content.Context) {
        audio = context.getSystemService(AudioManager::class.java)
        refresh()
        store.setting("dictation")?.toLongOrNull()?.let(store::dictation)?.let(::show)
    }

    private fun refresh() {
        list.clear()
        list += store.dictations()
    }

    /** The text as saved. */
    fun text(): String = typing?.text?.trim() ?: doc.text()

    fun create() = leave {
        refresh()
        show(store.dictation(store.createDictation()) ?: return@leave)
    }

    fun openDictation(id: Long) {
        if (open?.id == id) return
        leave { store.dictation(id)?.let(::show) }
    }

    /** Back to the threads. */
    fun close() = leave {
        open = null
        store.setSetting("dictation", "")
    }

    fun rename(id: Long, title: String) {
        val t = title.trim()
        if (t.isEmpty()) return
        store.renameDictation(id, t)
        refresh()
        if (open?.id == id) open = store.dictation(id)
    }

    fun delete(id: Long) {
        if (open?.id == id) {
            session?.cancel()
            session = null
            listening = false
            open = null
            store.setSetting("dictation", "")
        }
        store.deleteDictation(id)
        refresh()
    }

    private fun show(d: DictationInfo) {
        open = d
        doc.load(d.text)
        relayout()
        store.setSetting("dictation", "${d.id}")
    }

    /** Finish listening or typing and save, forget the open one if it's empty, then [next]. */
    private fun leave(next: () -> Unit) {
        dictateMode(resume = false)
        val d = open
        val go = {
            if (d != null && store.dictation(d.id)?.text?.isEmpty() == true) {
                store.deleteDictation(d.id)
                refresh()
            }
            next()
        }
        if (session != null || finishing) {
            then = go
            stop()
        } else {
            go()
        }
    }

    /** Start listening, new speech going in at the cursor. Returns why it can't, or null. */
    fun start(): String? {
        dictateMode(resume = false)
        if (open == null || session != null || finishing) return null
        val models = Scribe.claimForDictation()
        if (models == 0L) return if (Scribe.busy()) "Scribe is recording; stop it first." else "Scribe is busy."
        // A new session's clock starts at zero: fold the last one in first.
        doc.settle()
        session = Dictation(
            models, audio, Scribe.boost, Scribe.route, main,
            onChange = { paras ->
                doc.paras = paras
                relayout()
                saveSoon()
            },
            onLevel = { level = it },
        ).also { it.start() }
        listening = true
        relayout()
        return null
    }

    /** Stop listening; the text is saved once the last words are transcribed. */
    fun stop() {
        val s = session ?: return
        session = null
        listening = false
        level = 0f
        finishing = true
        s.finish { final ->
            doc.paras = final
            doc.settle()
            finishing = false
            relayout()
            save()
            then?.let {
                then = null
                it()
            }
        }
    }

    /** The app left the screen: the mic can't stay on in the background. */
    fun onHidden() {
        stop()
        save()
    }

    /**
     * Type instead: listening stops, and the text becomes a field for the
     * phone's keyboard with its cursor where the dictation cursor was.
     */
    fun typeMode() {
        if (open == null || typing != null) return
        if (session != null) listenAfterTyping = true
        if (session != null || finishing) {
            then = ::typeMode
            stop()
            return
        }
        // The text without the cursor's own space, the cursor where it stood.
        val c = laid.caret
        val before = if (c >= 0) laid.text.substring(0, c).trimEnd(' ') else laid.text
        val after = if (c >= 0) laid.text.substring(c + 1).trimStart(' ') else ""
        val gap = if (before.isNotEmpty() && after.isNotEmpty() && !before.endsWith('\n') && !after.startsWith('\n')) " " else ""
        val text = before + gap + after
        typing = TextFieldValue(text, TextRange(before.length + gap.length))
    }

    /**
     * Back to dictating: what was typed is the text, the cursor where the
     * keyboard's was. If listening when typing began, and [resume], it
     * starts again.
     */
    fun dictateMode(resume: Boolean = true) {
        val t = typing ?: return
        typing = null
        val listen = listenAfterTyping && resume
        listenAfterTyping = false
        val head = t.text.substring(0, t.selection.min)
        // A cursor within a word counts as after it; one at a line's start
        // as past the break before that line.
        var cursor = DictationDoc.items(head).size
        if (head.trimEnd(' ', '\t').endsWith('\n') && cursor > 0 && t.text.substring(head.length).isNotBlank()) cursor++
        doc.load(t.text, cursor)
        relayout()
        save()
        if (listen) start()
    }

    /** Typed: [value] is the field's new state. */
    fun type(value: TextFieldValue) {
        if (typing == null) return
        typing = value
        saveSoon()
    }

    private fun now(): Long = session?.breakNow() ?: 0

    /** Send new speech to the gap between words nearest [offset] in [laid]'s text. */
    fun moveCursor(offset: Int) {
        val (tok, after) = nearestGap(laid.toks.filter { it.second.placed }, offset) ?: return
        if (doc.moveCursor(built, tok, after, ::now)) relayout()
    }

    /** A new paragraph at the cursor (never two in a row, nor one first). */
    fun paragraphBreak() {
        if (doc.paragraphBreak(built, ::now)) changed()
    }

    /** ⌫: delete the sentence before the cursor (or the break right before it). */
    fun deleteSentence() {
        if (doc.deleteSentence(built)) changed()
    }

    fun delete(picked: List<Tok>) {
        doc.delete(picked)
        changed()
    }

    private fun changed() {
        relayout()
        saveSoon()
    }

    private fun relayout() {
        // Live text is ALL CAPS (Zipformer).
        built = doc.build().map { if (it.partial) it.withText(it.text.lowercase()) else it }
        // A non-breaking space for the cursor to stand on: a plain one would
        // be taken for the space between words.
        laid = doc.lay(built, " ")
    }

    private fun saveSoon() {
        main.removeCallbacks(saver)
        main.postDelayed(saver, 2_000)
    }

    private fun save() {
        main.removeCallbacks(saver)
        val d = open ?: return
        val text = text()
        if (text == store.dictation(d.id)?.text) return
        store.saveDictation(d.id, text)
        refresh()
        open = store.dictation(d.id)
    }
}
