package com.enigmacurry.voxscribe

import android.annotation.SuppressLint
import android.content.Context
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.SystemClock
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

private const val TAG = "VoxScribe"
private const val RATE = 16_000
/** Threads per recognizer (Parakeet and Zipformer each). */
private const val THREADS = 4

data class ClipUi(val text: String, val partial: Boolean)

data class Para(val id: String, val clips: List<ClipUi>) {
    val text: String get() = clips.filter { it.text.isNotBlank() }.joinToString(" ") { it.text }
}

sealed interface Phase {
    /** Models missing; nothing started yet, or the last attempt failed. */
    data class NeedModels(val error: String? = null, val resume: Boolean = false) : Phase
    data class Installing(val step: Step, val overall: Float) : Phase
    /** Installed but not in memory (not loaded yet, or released while idle). */
    data object Unloaded : Phase
    data object Loading : Phase
    data object Ready : Phase
    data class Failed(val error: String) : Phase
}

/**
 * Process-wide dictation state: the loaded models, the running session,
 * the microphone and the transcript. Lives as long as the process, so
 * the UI coming and going (app switches, rotation) never reloads the
 * models; [ScribeService] keeps the process alive and in the foreground
 * while recording and for a while after.
 */
object Scribe {
    lateinit var store: ModelStore
        private set
    private lateinit var app: Context
    private lateinit var saved: File
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    var phase by mutableStateOf<Phase>(Phase.Unloaded)
        private set
    /** A session is open (capturing or paused). */
    var recording by mutableStateOf(false)
        private set
    var paused by mutableStateOf(false)
        private set
    /** Stopped, but passes 2 and 3 are still draining. */
    var finishing by mutableStateOf(false)
        private set
    var level by mutableFloatStateOf(0f)
        private set
    val paragraphs = mutableStateListOf<Para>()

    /** An activity is on screen. */
    @Volatile var visible = false
        private set
    /** Last moment the app was on screen or recording ([SystemClock.elapsedRealtime]). */
    @Volatile var lastActive = 0L
        private set

    @Volatile private var models = 0L
    @Volatile private var session = 0L
    /** The running mic thread, and stopped ones that may still be in their last read. */
    private var mic: Mic? = null
    private val stopping = mutableListOf<Mic>()
    private var poller: Job? = null
    private var saver: Job? = null
    private val sessionIds = mutableSetOf<String>()

    /** Recorded time of the open session, excluding pauses. */
    private var activeMs = 0L
    private var runStart = 0L

    fun init(context: Context) {
        if (::app.isInitialized) return
        app = context.applicationContext
        store = ModelStore(app.noBackupFilesDir)
        saved = File(app.filesDir, "transcript.json")
        phase = if (store.ready()) Phase.Unloaded else Phase.NeedModels(resume = store.started())
        restore()
        scope.launch {
            InstallService.state.collect { s ->
                when (s) {
                    is Install.Running -> phase = Phase.Installing(s.step, s.overall)
                    is Install.Failed ->
                        if (phase is Phase.Installing) phase = Phase.NeedModels(s.error, resume = true)
                    Install.Done -> if (phase is Phase.Installing) load()
                    Install.Idle -> {}
                }
            }
        }
    }

    /** The activity started or stopped. */
    fun onVisible(v: Boolean) {
        visible = v
        lastActive = SystemClock.elapsedRealtime()
        if (v && phase == Phase.Unloaded) load()
    }

    fun install() = InstallService.start(app)

    fun load() {
        if (phase == Phase.Loading || models != 0L) return
        phase = Phase.Loading
        scope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching { Native.load(store.dir.path, THREADS) }
            }
            result.onSuccess {
                models = it
                phase = Phase.Ready
                // Keeps the models warm (and the process alive) after the app is left.
                ScribeService.update(app)
            }.onFailure {
                Log.e(TAG, "model load failed", it)
                phase = Phase.Failed(it.message ?: it.toString())
            }
        }
    }

    /** Free the models if nothing is using them; they reload when the app is next opened. */
    fun release() {
        if (models == 0L || recording || finishing) return
        Native.freeModels(models)
        models = 0
        phase = Phase.Unloaded
        Log.i(TAG, "models released")
    }

    fun loaded() = models != 0L

    /** Caller has checked RECORD_AUDIO and is in the foreground. */
    fun start() {
        if (models == 0L || recording || finishing) return
        session = Native.start(models, RATE)
        sessionIds.clear()
        recording = true
        paused = false
        activeMs = 0
        ScribeService.update(app)
        startCapture()
        val s = session
        poller = scope.launch(Dispatchers.IO) {
            while (isActive && recording) {
                val events = Native.poll(s, 100)
                withContext(Dispatchers.Main) { apply(events) }
            }
        }
    }

    private class Mic(s: Long) {
        @Volatile var run = true
        val thread = Thread({ capture(s, this) }, "vox-mic").apply { start() }
    }

    private fun startCapture() {
        runStart = SystemClock.elapsedRealtime()
        mic = Mic(session)
    }

    private fun stopCapture() {
        mic?.let {
            it.run = false
            stopping += it
        }
        mic = null
    }

    /** Stop the microphone but keep the session open. */
    fun pause() {
        if (!recording || paused || finishing) return
        paused = true
        activeMs += SystemClock.elapsedRealtime() - runStart
        stopCapture()
        level = 0f
        // Close the utterance that was cut off.
        Native.breakParagraph(session)
        ScribeService.update(app)
    }

    fun resume() {
        if (!recording || !paused || finishing) return
        paused = false
        startCapture()
        ScribeService.update(app)
    }

    /** Elapsed recording time of the open session, excluding pauses. */
    fun elapsedMs(): Long =
        activeMs + if (recording && !paused) SystemClock.elapsedRealtime() - runStart else 0

    @SuppressLint("MissingPermission")
    private fun capture(s: Long, m: Mic) {
        val min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_FLOAT)
        val rec = AudioRecord(
            MediaRecorder.AudioSource.VOICE_RECOGNITION,
            RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_FLOAT,
            maxOf(min, RATE * 4), // ~250 ms of float samples
        )
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            Log.e(TAG, "AudioRecord failed to initialize")
            rec.release()
            return
        }
        val buf = FloatArray(RATE / 10) // 100 ms
        rec.startRecording()
        try {
            while (m.run) {
                val n = rec.read(buf, 0, buf.size, AudioRecord.READ_BLOCKING)
                if (n > 0) Native.push(s, buf, n)
            }
        } finally {
            rec.stop()
            rec.release()
        }
    }

    /** Stop the mic, then let the engine finish its passes in the background. */
    fun stop() {
        if (!recording || finishing) return
        if (!paused) activeMs += SystemClock.elapsedRealtime() - runStart
        finishing = true
        val s = session
        ScribeService.update(app)
        stopCapture()
        val mics = stopping.toList()
        stopping.clear()
        // Not cancellable: the session must be finished (and freed) exactly once.
        scope.launch(Dispatchers.IO + NonCancellable) {
            // No mic thread may push into the session once it is finished.
            mics.forEach { it.thread.join() }
            withContext(Dispatchers.Main) { recording = false }
            poller?.join()
            poller = null
            val transcript = Native.finish(s)
            withContext(Dispatchers.Main) {
                applyFinal(transcript)
                session = 0
                level = 0f
                paused = false
                finishing = false
                lastActive = SystemClock.elapsedRealtime()
                save(now = true)
                ScribeService.update(app)
            }
        }
    }

    fun breakParagraph() {
        if (recording && !paused) Native.breakParagraph(session)
    }

    fun clear() {
        if (recording || finishing) return
        paragraphs.clear()
        save(now = true)
    }

    fun text(): String = paragraphs.map { it.text }.filter { it.isNotBlank() }.joinToString("\n\n")

    /** The last finished (non-partial) text, for the notification. */
    fun lastLine(): String =
        paragraphs.asReversed().firstNotNullOfOrNull { p ->
            p.clips.filter { !it.partial && it.text.isNotBlank() }
                .takeIf { it.isNotEmpty() }
                ?.joinToString(" ") { it.text }
        } ?: ""

    private fun apply(json: String) {
        val events = JSONArray(json)
        var changed = false
        for (i in 0 until events.length()) {
            val ev = events.getJSONObject(i)
            when (ev.getString("t")) {
                "paragraph" -> {
                    upsert(parse(ev.getJSONObject("p")))
                    if (ev.getString("change") != "partial") changed = true
                }
                "removed" -> {
                    paragraphs.removeAll { it.id == ev.getString("id") }
                    changed = true
                }
                "level" -> if (!paused) level = ev.getDouble("rms").toFloat()
            }
        }
        if (changed) save(now = false)
    }

    private fun applyFinal(json: String) {
        val list = JSONObject(json).optJSONArray("paragraphs") ?: JSONArray()
        val final = (0 until list.length()).map { parse(list.getJSONObject(it)) }
        val keep = final.map { it.id }.toSet()
        paragraphs.removeAll { it.id in sessionIds && it.id !in keep }
        final.forEach(::upsert)
    }

    private fun upsert(p: Para) {
        sessionIds += p.id
        val i = paragraphs.indexOfFirst { it.id == p.id }
        if (i >= 0) paragraphs[i] = p else paragraphs += p
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

    /**
     * Write the finished text to `files/transcript.json`, so a killed
     * process doesn't lose it. Partials are left out. Debounced unless [now].
     */
    private fun save(now: Boolean) {
        val snapshot = JSONArray()
        for (p in paragraphs) {
            val text = p.clips.filter { !it.partial && it.text.isNotBlank() }.joinToString(" ") { it.text }
            if (text.isNotEmpty()) snapshot.put(JSONObject().put("id", p.id).put("text", text))
        }
        saver?.cancel()
        saver = scope.launch {
            if (!now) delay(2_000)
            withContext(Dispatchers.IO + NonCancellable) {
                val tmp = File(saved.path + ".tmp")
                tmp.writeText(snapshot.toString())
                tmp.renameTo(saved)
            }
        }
    }

    private fun restore() {
        val list = runCatching { JSONArray(saved.readText()) }.getOrNull() ?: return
        for (i in 0 until list.length()) {
            val o = list.getJSONObject(i)
            paragraphs += Para(o.getString("id"), listOf(ClipUi(o.getString("text"), partial = false)))
        }
    }
}
