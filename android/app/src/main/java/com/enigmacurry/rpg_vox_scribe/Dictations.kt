package com.enigmacurry.rpg_vox_scribe

import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Dictations: text written by voice and edited in place, as in the voice
 * keyboard, but saved, so you can come back and keep going. Unlike a
 * thread's transcript, which runs in the order things were said, a tap
 * sends new speech anywhere in the text. No audio is kept.
 *
 * The open one's text is a [DictationDoc]; a [Dictation] adds to it while
 * listening. Each session folds into the text when it ends, which is
 * saved then and a moment after each change. Main thread only.
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
    var paused by mutableStateOf(false)
        private set
    /** Stopped, the last words still being transcribed. */
    var finishing by mutableStateOf(false)
        private set
    /** The words being retyped, null when not. */
    var retyping by mutableStateOf<String?>(null)
        private set

    private val doc = DictationDoc()
    /** [doc] as last built. */
    private var built: List<Tok> = emptyList()
    private var session: Dictation? = null
    private var retype: Retype? = null
    private var retypeWasPaused = false
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
    fun text(): String = doc.text()

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

    /** Finish listening and save, forget the open one if it's empty, then [next]. */
    private fun leave(next: () -> Unit) {
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
        if (open == null || session != null || finishing) return null
        val models = Scribe.claimForDictation()
        if (models == 0L) return if (Scribe.busy()) "Scribe is recording; stop it first." else "Scribe is busy."
        // A new session's clock starts at zero: fold the last one in first.
        doc.settle()
        session = Dictation(models, audio, Scribe.boost, Scribe.route, main) { paras ->
            doc.paras = paras
            relayout()
            saveSoon()
        }.also { it.start() }
        listening = true
        paused = false
        relayout()
        return null
    }

    /** Stop listening; the text is saved once the last words are transcribed. */
    fun stop() {
        val s = session ?: return
        session = null
        listening = false
        paused = false
        finishing = true
        retype = null
        retyping = null
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

    fun togglePause() {
        val s = session ?: return
        s.paused = !s.paused
        paused = s.paused
    }

    /** The app left the screen: the mic can't stay on in the background. */
    fun onHidden() = stop()

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

    fun delete(picked: List<Tok>) {
        doc.delete(picked)
        changed()
    }

    /** Retype [picked]: listening pauses and their words go to [retyping]; [endRetype] finishes. */
    fun beginRetype(picked: List<Tok>) {
        if (retype != null) return
        val r = doc.editOf(picked) ?: return
        retype = r
        val s = session
        retypeWasPaused = s?.paused ?: true
        s?.paused = true
        paused = s != null
        retyping = r.text
    }

    /** Replace the words being retyped with [text], or leave them if null; listening resumes unless it was paused before. */
    fun endRetype(text: String?) {
        val r = retype ?: return
        retype = null
        retyping = null
        if (text != null) {
            doc.retype(r, text)
            changed()
        }
        session?.let {
            it.paused = retypeWasPaused
            paused = it.paused
        }
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
        val text = doc.text()
        if (text == store.dictation(d.id)?.text) return
        store.saveDictation(d.id, text)
        refresh()
        open = store.dictation(d.id)
    }
}
