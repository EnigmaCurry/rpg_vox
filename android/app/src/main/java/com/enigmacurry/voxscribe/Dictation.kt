package com.enigmacurry.voxscribe

import android.annotation.SuppressLint
import android.app.Application
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
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
    data object Loading : Phase
    data object Ready : Phase
    data class Failed(val error: String) : Phase
}

class DictationViewModel(app: Application) : AndroidViewModel(app) {
    val store = ModelStore(app.noBackupFilesDir)

    var phase by mutableStateOf<Phase>(
        if (store.ready()) Phase.Loading else Phase.NeedModels(resume = store.started()),
    )
        private set
    var recording by mutableStateOf(false)
        private set
    /** Stopped, but passes 2 and 3 are still draining. */
    var finishing by mutableStateOf(false)
        private set
    var level by mutableFloatStateOf(0f)
        private set
    val paragraphs = mutableStateListOf<Para>()

    @Volatile private var models = 0L
    @Volatile private var session = 0L
    @Volatile private var capturing = false
    private var audio: Thread? = null
    private var poller: Job? = null
    private var stopper: Job? = null
    private val sessionIds = mutableSetOf<String>()

    init {
        if (phase == Phase.Loading) loadModels()
        viewModelScope.launch {
            InstallService.state.collect { s ->
                when (s) {
                    is Install.Running -> phase = Phase.Installing(s.step, s.overall)
                    is Install.Failed -> if (phase is Phase.Installing) phase = Phase.NeedModels(s.error, resume = true)
                    Install.Done -> if (phase is Phase.Installing) loadModels()
                    Install.Idle -> {}
                }
            }
        }
    }

    fun install() = InstallService.start(getApplication())

    private fun loadModels() {
        phase = Phase.Loading
        viewModelScope.launch(Dispatchers.IO) {
            val result = runCatching { Native.load(store.dir.path, THREADS) }
            withContext(Dispatchers.Main) {
                result.onSuccess {
                    models = it
                    phase = Phase.Ready
                }.onFailure {
                    Log.e(TAG, "model load failed", it)
                    phase = Phase.Failed(it.message ?: it.toString())
                }
            }
        }
    }

    /** Caller has checked RECORD_AUDIO. */
    @SuppressLint("MissingPermission")
    fun start() {
        if (models == 0L || recording || finishing) return
        val s = Native.start(models, RATE)
        session = s
        sessionIds.clear()
        stopper = null
        recording = true
        capturing = true
        audio = Thread({ capture(s) }, "vox-mic").apply { start() }
        poller = viewModelScope.launch(Dispatchers.IO) {
            while (isActive && recording) {
                val events = Native.poll(s, 100)
                withContext(Dispatchers.Main) { apply(events) }
            }
        }
    }

    @SuppressLint("MissingPermission")
    private fun capture(s: Long) {
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
            while (capturing) {
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
        finishing = true
        val s = session
        // Not cancellable: the session must be finished (and freed) exactly once.
        stopper = viewModelScope.launch(Dispatchers.IO + NonCancellable) {
            capturing = false
            audio?.join()
            audio = null
            withContext(Dispatchers.Main) { recording = false }
            poller?.join()
            poller = null
            val transcript = Native.finish(s)
            withContext(Dispatchers.Main) {
                applyFinal(transcript)
                session = 0
                level = 0f
                finishing = false
            }
        }
    }

    fun breakParagraph() {
        if (recording) Native.breakParagraph(session)
    }

    fun clear() {
        if (!recording && !finishing) paragraphs.clear()
    }

    fun text(): String = paragraphs.map { it.text }.filter { it.isNotBlank() }.joinToString("\n\n")

    private fun apply(json: String) {
        val events = JSONArray(json)
        for (i in 0 until events.length()) {
            val ev = events.getJSONObject(i)
            when (ev.getString("t")) {
                "paragraph" -> upsert(parse(ev.getJSONObject("p")))
                "removed" -> paragraphs.removeAll { it.id == ev.getString("id") }
                "level" -> level = ev.getDouble("rms").toFloat()
            }
        }
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

    override fun onCleared() {
        capturing = false
        recording = false
        val audio = audio
        val poller = poller
        val stopper = stopper
        // Off the main thread: finishing a session waits for its passes.
        Thread {
            audio?.join()
            runBlocking {
                poller?.join()
                stopper?.join()
            }
            if (stopper == null && session != 0L) Native.finish(session)
            session = 0
            if (models != 0L) Native.freeModels(models)
            models = 0
        }.start()
    }
}
