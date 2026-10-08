package com.enigmacurry.rpg_vox_scribe

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Color
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
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
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

/**
 * Voice keyboard: pick Scribe as the input method in any app and it
 * starts listening, shows the live transcript (partials dimmed), and
 * Insert types the finished text into the field once passes 2 and 3 are
 * done, then switches back to the previous keyboard. Uses the models
 * the app loaded, loading them if needed.
 */
class ScribeKeyboard : InputMethodService() {
    private val main = Handler(Looper.getMainLooper())

    private lateinit var status: TextView
    private lateinit var transcript: TextView
    private lateinit var scroll: ScrollView
    private lateinit var insertButton: Button

    private var session: Dictation? = null
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
            setTextIsSelectable(false)
        }
        scroll = ScrollView(this).apply {
            addView(transcript, MATCH_PARENT, WRAP_CONTENT)
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
        if (session == null && !waiting) begin()
    }

    override fun onFinishInputView(finishingInput: Boolean) {
        // Hidden or moved to another field: drop what was being said.
        waiting = false
        session?.cancel()
        session = null
        super.onFinishInputView(finishingInput)
    }

    override fun onDestroy() {
        session?.cancel()
        session = null
        super.onDestroy()
    }

    private fun begin() {
        transcript.text = ""
        insertButton.isEnabled = false
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
        session = Dictation(
            models,
            getSystemService(AudioManager::class.java),
            Scribe.boost,
            Scribe.route,
            main,
            onChange = ::render,
        ).also { it.start() }
    }

    private fun render(paras: List<Para>) {
        val dim = 0xFF888888.toInt()
        val sb = SpannableStringBuilder()
        for (p in paras) {
            val clips = p.clips.filter { it.text.isNotBlank() }
            if (clips.isEmpty()) continue
            if (sb.isNotEmpty()) sb.append("\n\n")
            clips.forEachIndexed { i, c ->
                if (i > 0) sb.append(' ')
                val at = sb.length
                sb.append(c.text)
                if (c.partial) sb.setSpan(ForegroundColorSpan(dim), at, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
        }
        transcript.text = sb
        scroll.post { scroll.fullScroll(View.FOCUS_DOWN) }
    }

    private fun insert() {
        val s = session ?: return
        session = null
        insertButton.isEnabled = false
        status.text = "Finishing…"
        s.finish { paras ->
            val ic = currentInputConnection
            val multiLine = (currentInputEditorInfo?.inputType ?: 0) and EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE != 0
            var text = paras.map { it.text.trim() }.filter { it.isNotEmpty() }
                .joinToString(if (multiLine) "\n\n" else " ")
            if (ic != null && text.isNotEmpty()) {
                val before = ic.getTextBeforeCursor(1, 0)
                if (!before.isNullOrEmpty() && !before.last().isWhitespace()) text = " $text"
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
    private val onChange: (List<Para>) -> Unit,
) {
    private var session = 0L
    @Volatile private var capturing = true
    @Volatile private var polling = true
    @Volatile private var ended = false
    private var capture: Thread? = null
    private var poller: Thread? = null
    private val paras = LinkedHashMap<String, Para>()

    fun start() {
        session = Native.start(models, RATE, true)
        poller = Thread({
            while (polling) {
                val events = Native.poll(session, 100)
                if (events.length > 2) main.post { apply(events) }
            }
        }, "ime-poll").apply { start() }
        capture = Thread(::record, "ime-mic").apply { start() }
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
        try {
            while (capturing) {
                val n = rec.read(shorts, 0, shorts.size, AudioRecord.READ_BLOCKING)
                if (n <= 0) continue
                for (i in 0 until n) buf[i] = shorts[i] / 32768f
                gain.process(buf, n)
                Native.push(session, buf, n)
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
        for (i in 0 until events.length()) {
            val ev = events.getJSONObject(i)
            when (ev.getString("t")) {
                "paragraph" -> parse(ev.getJSONObject("p")).let { paras[it.id] = it }
                "removed" -> paras.remove(ev.getString("id"))
            }
        }
        onChange(paras.values.toList())
    }

    /** Stop listening, let the passes finish, then hand back the final paragraphs on [main]. */
    fun finish(done: (List<Para>) -> Unit) = end { json ->
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

    private fun parse(o: JSONObject): Para {
        val clips = o.getJSONArray("clips")
        return Para(
            id = o.getString("id"),
            clips = (0 until clips.length()).map {
                val c = clips.getJSONObject(it)
                ClipUi(c.getString("text"), c.getString("stage") == "partial")
            },
        )
    }
}
