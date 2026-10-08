package com.enigmacurry.rpg_vox_scribe

import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioManager
import android.media.AudioRecord
import android.media.audiofx.AutomaticGainControl
import android.media.MediaRecorder
import android.media.projection.MediaProjection
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.OpenableColumns
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
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

private const val TAG = "Scribe"
private const val RATE = 16_000
/** Threads per recognizer (Parakeet and Zipformer each). */
private const val THREADS = 4
/** Louder than this (RMS dBFS) is sound, when trimming a recording with no words. */
private const val SOUND_DB = -45f
/** Kept around the sound when trimming. */
private const val SOUND_PAD_MS = 300L
/** How far file decoding may run ahead of pass 2, bounding queued audio. */
private const val FILE_AHEAD_MS = 120_000L

data class ClipUi(val text: String, val partial: Boolean)

/**
 * One paragraph. [source] is its speaker ("Me", "Phone") when a recording
 * has two sources. With kept audio, [audio] is the file it was heard in
 * and [words] its words' times there.
 */
data class Para(
    val id: String,
    val clips: List<ClipUi>,
    val source: String? = null,
    val audio: String? = null,
    val words: List<Word> = emptyList(),
    /** When it was said (epoch ms); the transcript is in this order. */
    val atMs: Long = 0,
) {
    val text: String get() = clips.filter { it.text.isNotBlank() }.joinToString(" ") { it.text }
    /** [text]'s words as shown. */
    val shown: List<String> by lazy { text.split(' ').filter { it.isNotEmpty() } }
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

/** What a live recording listens to. */
enum class Mode(val mic: Boolean, val phone: Boolean) {
    MIC(true, false),
    PHONE(false, true),
    BOTH(true, true),
}

/** A file being transcribed. [progress] is the share of its audio through pass 2. */
data class FileJob(val name: String, val progress: Float)

/**
 * Process-wide dictation state: the loaded models, the running sources,
 * their audio, and the transcript. Lives as long as the process, so the
 * UI coming and going (app switches, rotation) never reloads the models;
 * [ScribeService] keeps the process alive and in the foreground while
 * recording or transcribing a file, and for a while after.
 */
object Scribe {
    lateinit var store: ModelStore
        private set
    private lateinit var app: Context
    private lateinit var threads: ThreadStore
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val main = Handler(Looper.getMainLooper())

    var phase by mutableStateOf<Phase>(Phase.Unloaded)
        private set
    var mode by mutableStateOf(Mode.MIC)
    /** Mic gain and which mic; applied when the mic (re)starts. */
    var boost by mutableStateOf(Boost.AUTO)
        private set
    var route by mutableStateOf(MicRoute.AUTO)
        private set
    /** Save each recording's audio (.opus) with its transcript, for playback. */
    var keepAudio by mutableStateOf(false)
        private set
    /** The input the mic source is recording from, while it runs. */
    var micName by mutableStateOf<String?>(null)
        private set
    /** A live recording is open (capturing or paused). */
    var recording by mutableStateOf(false)
        private set
    /** The mode of the open recording. */
    var recordingMode by mutableStateOf(Mode.MIC)
        private set
    var paused by mutableStateOf(false)
        private set
    /** Stopped, but passes 2 and 3 are still draining. */
    var finishing by mutableStateOf(false)
        private set
    var file by mutableStateOf<FileJob?>(null)
        private set
    var level by mutableFloatStateOf(0f)
        private set
    /** The open thread's transcript. */
    val paragraphs = mutableStateListOf<Para>()
    /** The open thread. */
    var thread by mutableStateOf(ThreadInfo(0, "", "", "", 0))
        private set
    /** The open thread's kept audio: file to the recording it's part of. */
    var threadAudio by mutableStateOf<Map<String, String>>(emptyMap())
        private set
    /** The open thread's recordings, oldest first. */
    val recordings: List<String> get() = threadAudio.values.distinct().sorted()
    /** Every thread, the most recently active first. */
    val threadList = mutableStateListOf<ThreadInfo>()
    /** Paragraph id to its stored row, as last written. */
    private val written = HashMap<String, Saved>()

    /** An activity is on screen. */
    @Volatile var visible = false
        private set
    /** Last moment the app was on screen or busy ([SystemClock.elapsedRealtime]). */
    @Volatile var lastActive = 0L
        private set

    @Volatile private var models = 0L
    private val sources = mutableListOf<Source>()
    private var saver: Job? = null
    private var pendingFile: Uri? = null
    @Volatile private var cancelFile = false

    /** Consent for phone audio, waiting for [ScribeService] to turn it into a projection. */
    var pendingProjection: Pair<Int, Intent>? = null
        private set
    private var projection: MediaProjection? = null

    /** Recorded time of the open recording, excluding pauses. */
    @Volatile private var activeMs = 0L
    @Volatile private var runStart = 0L

    fun busy() = recording || finishing || file != null

    /** The voice keyboard ([ScribeKeyboard]) holds the mic and the models. */
    @Volatile var dictating = false
        private set

    /** The loaded models for the keyboard, marking them in use; 0 if not loaded or Scribe is busy. */
    fun claimForKeyboard(): Long {
        if (models == 0L || busy()) return 0
        dictating = true
        lastActive = SystemClock.elapsedRealtime()
        return models
    }

    fun releaseFromKeyboard() {
        dictating = false
        lastActive = SystemClock.elapsedRealtime()
    }

    fun init(context: Context) {
        if (::app.isInitialized) return
        app = context.applicationContext
        store = ModelStore(app.noBackupFilesDir)
        threads = ThreadStore(File(app.filesDir, "scribe.db"))
        val prefs = app.getSharedPreferences("scribe", Context.MODE_PRIVATE)
        mode = runCatching { Mode.valueOf(prefs.getString("mode", "MIC")!!) }.getOrDefault(Mode.MIC)
        if (Build.VERSION.SDK_INT < 29) mode = Mode.MIC
        boost = runCatching { Boost.valueOf(prefs.getString("boost", "AUTO")!!) }.getOrDefault(Boost.AUTO)
        route = runCatching { MicRoute.valueOf(prefs.getString("route", "AUTO")!!) }.getOrDefault(MicRoute.AUTO)
        keepAudio = prefs.getBoolean("keep_audio", false)
        phase = if (store.ready()) Phase.Unloaded else Phase.NeedModels(resume = store.started())
        migrate()
        show(threads.current())
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

    fun chooseBoost(b: Boost) {
        boost = b
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putString("boost", b.name).apply()
    }

    fun chooseKeepAudio(keep: Boolean) {
        keepAudio = keep
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putBoolean("keep_audio", keep).apply()
    }

    /** Where kept recordings live. */
    val audioDir: File get() = File(app.filesDir, "audio")

    /**
     * The open thread's recordings end to end, oldest first, as one
     * timeline: each file with where it plays on it, and where each
     * recording starts. A recording with no words is cut to the stretch
     * from its first sound to its last (quiet inside it stays). Fills in
     * lengths and sound spans not known yet.
     */
    private fun timeline(): Pair<List<AudioFile>, Map<String, Long>> {
        val files = mutableListOf<AudioFile>()
        val starts = HashMap<String, Long>()
        val heard = paragraphs.mapNotNullTo(HashSet()) { it.audio }
        var at = 0L
        for ((rec, rows) in threads.audioRows(thread.id).groupBy { it.recording }) {
            val base = rows.minOf { it.offsetMs }
            // The recording's own time of each file: (start, length).
            val placed = rows.map { r ->
                val len = r.lengthMs.takeIf { it > 0 } ?: audioLength(r.file).also { threads.setAudioLength(r.file, it) }
                Triple(r, r.offsetMs - base, len)
            }
            var from = 0L
            var to = placed.maxOf { (_, off, len) -> off + len }
            if (rows.none { it.file in heard }) {
                val sound = placed.mapNotNull { (r, off, len) ->
                    val (a, b) = if (r.soundStartMs >= 0) {
                        r.soundStartMs to r.soundEndMs
                    } else {
                        (audioSound(r.file) ?: (0L to len)).also { threads.setAudioSound(r.file, it.first, it.second) }
                    }
                    (off + a to off + b).takeIf { b > a }
                }
                if (sound.isNotEmpty()) {
                    from = sound.minOf { it.first }
                    to = sound.maxOf { it.second }
                }
            }
            for ((r, off, _) in placed) files += AudioFile(r.file, at + off - from, at + to - from)
            starts[rec] = at
            at += to - from
        }
        return files to starts
    }

    /** First to last sound in a kept file (ms), or null if it's all quiet. */
    private fun audioSound(file: String): Pair<Long, Long>? =
        runCatching {
            val h = Native.playerOpen(File(audioDir, file).path)
            try {
                Native.playerSpan(h, SOUND_DB, SOUND_PAD_MS)?.let { it[0] to it[1] }
            } finally {
                Native.playerFree(h)
            }
        }.getOrNull()

    /** Length of a kept file (ms), 0 if it can't be read. */
    private fun audioLength(file: String): Long =
        runCatching {
            val h = Native.playerOpen(File(audioDir, file).path)
            try {
                Native.playerLength(h)
            } finally {
                Native.playerFree(h)
            }
        }.getOrDefault(0L)

    /** Play the open thread's audio, every recording in turn, from the start of [recording]. */
    fun playFromStart(recording: String) {
        if (busy()) return
        val (files, starts) = timeline()
        Playback.play(audioDir, files, starts[recording] ?: return)
    }

    /** Play the open thread's audio, every recording in turn, from [word] of [p]. */
    fun play(p: Para, word: Word) {
        if (busy()) return
        val (files, _) = timeline()
        val offset = files.firstOrNull { it.file == p.audio }?.offsetMs ?: return
        Playback.play(audioDir, files, offset + word.startMs)
    }

    fun chooseRoute(r: MicRoute) {
        route = r
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putString("route", r.name).apply()
    }

    fun choose(m: Mode) {
        if (recording) return
        mode = m
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putString("mode", m.name).apply()
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
                // Starts the idle clock: loaded by the keyboard, the app may never have been on screen.
                lastActive = SystemClock.elapsedRealtime()
                // Keeps the models warm (and the process alive) after the app is left.
                ScribeService.update(app)
                pendingFile?.let { uri ->
                    pendingFile = null
                    startFile(uri)
                }
            }.onFailure {
                Log.e(TAG, "model load failed", it)
                phase = Phase.Failed(it.message ?: it.toString())
            }
        }
    }

    /** Free the models if nothing is using them; they reload when the app is next opened. */
    fun release() {
        if (models == 0L || busy() || dictating) return
        Native.freeModels(models)
        models = 0
        phase = Phase.Unloaded
        Log.i(TAG, "models released")
    }

    fun loaded() = models != 0L

    // ---- sources -------------------------------------------------------

    private enum class Kind { MIC, PHONE, FILE }

    /** One engine session and what feeds it. */
    /** [audio]: the file this source's audio is kept in, part of [recording]. */
    private class Source(
        val kind: Kind,
        val label: String?,
        val session: Long,
        val audio: String?,
        val recording: String?,
    ) {
        /** Set once the first audio arrives. */
        @Volatile var started = false
        /** Epoch ms at the session's time zero, for ordering its paragraphs. */
        @Volatile var baseMs = 0L
        /** Paragraphs this session produced (to apply its final transcript). */
        val ids = mutableSetOf<String>()
        var capture: Capture? = null
        val stopping = mutableListOf<Capture>()
        @Volatile var polling = true
        var poller: Thread? = null
        var level = 0f
        /** Files: audio through pass 2, for progress and decode throttling. */
        @Volatile var doneMs = 0L
    }

    private class Capture(val thread: Thread) {
        @Volatile var run = true
    }

    /** Start a session for [kind]; with [recording], its audio is kept as part of that recording. */
    private fun open(kind: Kind, label: String?, streaming: Boolean, recording: String? = null): Source {
        var audio = recording?.let { "$it-${kind.name.lowercase()}.opus" }
        val session = if (audio == null) {
            Native.start(models, RATE, streaming, null)
        } else {
            audioDir.mkdirs()
            runCatching { Native.start(models, RATE, streaming, File(audioDir, audio).path) }
                .onFailure { Log.e(TAG, "can't keep audio", it) }
                .getOrElse {
                    audio = null
                    Native.start(models, RATE, streaming, null)
                }
        }
        val src = Source(kind, label, session, audio, recording)
        audio?.let { threads.addAudio(thread.id, recording!!, it) }
        sources += src
        src.poller = Thread({
            while (src.polling) {
                val events = Native.poll(src.session, 100)
                if (events.length > 2) main.post { apply(src, events) }
            }
        }, "vox-poll").apply { start() }
        return src
    }

    /** Stop polling, then free the session. Off the main thread. */
    private fun close(src: Source) {
        src.polling = false
        src.poller?.join()
        Native.free(src.session)
    }

    // ---- live recording ------------------------------------------------

    /**
     * Start a recording in [mode]. For phone audio, [consent] is the
     * MediaProjection consent result; capture starts once [ScribeService]
     * is in the foreground and hands the projection to [onProjection].
     * Caller has checked RECORD_AUDIO and is in the foreground.
     */
    fun start(consent: Pair<Int, Intent>? = null) {
        if (models == 0L || busy() || dictating) return
        val m = mode
        if (m.phone && consent == null) return
        Playback.stop()
        recordingMode = m
        sources.clear()
        val rec = if (keepAudio) newRecording() else null
        if (m.mic) open(Kind.MIC, if (m.phone) "Me" else null, streaming = true, rec)
        if (m.phone) open(Kind.PHONE, "Phone", streaming = true, rec)
        pendingProjection = consent
        recording = true
        paused = false
        activeMs = 0
        runStart = SystemClock.elapsedRealtime()
        wallStart = System.currentTimeMillis()
        ScribeService.update(app)
        startCaptures()
    }

    /** From [ScribeService], once it is a mediaProjection foreground service. */
    fun onProjection(mp: MediaProjection) {
        pendingProjection = null
        projection = mp
        mp.registerCallback(object : MediaProjection.Callback() {
            // The user stopped sharing from the system UI.
            override fun onStop() {
                main.post { if (projection === mp) stop() }
            }
        }, main)
        if (recording && !paused) startCaptures()
    }

    private fun startCaptures() {
        for (src in sources) {
            if (src.capture != null) continue
            when (src.kind) {
                Kind.MIC -> src.capture = micCapture(src)
                Kind.PHONE -> {
                    val mp = projection ?: continue
                    if (Build.VERSION.SDK_INT >= 29) src.capture = capture(src, make = { phoneRecord(mp) })
                }
                Kind.FILE -> {}
            }
        }
    }

    private fun stopCaptures() {
        for (src in sources) {
            src.capture?.let {
                it.run = false
                src.stopping += it
            }
            src.capture = null
        }
    }

    /** Stop listening but keep the recording open. */
    fun pause() {
        if (!recording || paused || finishing) return
        paused = true
        activeMs += SystemClock.elapsedRealtime() - runStart
        stopCaptures()
        level = 0f
        // Close the utterances that were cut off.
        sources.forEach { Native.breakParagraph(it.session) }
        ScribeService.update(app)
    }

    fun resume() {
        if (!recording || !paused || finishing) return
        paused = false
        runStart = SystemClock.elapsedRealtime()
        startCaptures()
        ScribeService.update(app)
    }

    /** Elapsed time of the open recording, excluding pauses. */
    fun elapsedMs(): Long =
        activeMs + if (recording && !paused) SystemClock.elapsedRealtime() - runStart else 0

    @SuppressLint("MissingPermission")
    private fun micRecord(): AudioRecord {
        val min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        return AudioRecord(
            MediaRecorder.AudioSource.VOICE_RECOGNITION,
            RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
            maxOf(min, RATE), // ~500 ms
        )
    }

    /** What other apps play (those that allow capture): media, games, unknown. */
    @SuppressLint("MissingPermission")
    private fun phoneRecord(mp: MediaProjection): AudioRecord {
        val config = AudioPlaybackCaptureConfiguration.Builder(mp)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .build()
        return AudioRecord.Builder()
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(RATE)
                    .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                    .build(),
            )
            .setBufferSizeInBytes(RATE) // ~500 ms
            .setAudioPlaybackCaptureConfig(config)
            .build()
    }

    /**
     * The mic: routed to a Bluetooth headset when [route] allows and one
     * is connected, through [MicGain], with its name in [micName].
     */
    private fun micCapture(src: Source): Capture {
        val routing = MicRouting(app.getSystemService(AudioManager::class.java))
        val gain = MicGain(boost)
        val useHeadset = route == MicRoute.AUTO
        return capture(
            src,
            make = {
                val headset = if (useHeadset) routing.headset() else null
                micRecord().also { rec -> headset?.let { rec.preferredDevice = it } }
            },
            process = gain::process,
            started = { rec ->
                // The headset's call link takes a moment; the name follows the switch.
                rec.addOnRoutingChangedListener({ r -> main.post { micName = (r as AudioRecord).inputName() } }, main)
                main.post { micName = rec.inputName() }
                Log.i(TAG, "mic: ${rec.inputName()}, boost ${boost.label}, platform AGC available: ${AutomaticGainControl.isAvailable()}")
            },
            ended = {
                routing.release()
                main.post { micName = null }
            },
        )
    }

    /** Read 100 ms blocks from [make]'s recorder into [src] until stopped. */
    private fun capture(
        src: Source,
        make: () -> AudioRecord,
        process: ((FloatArray, Int) -> Unit)? = null,
        started: (AudioRecord) -> Unit = {},
        ended: () -> Unit = {},
    ): Capture {
        lateinit var c: Capture
        c = Capture(Thread({
            val rec = runCatching(make).getOrNull()
            if (rec == null || rec.state != AudioRecord.STATE_INITIALIZED) {
                Log.e(TAG, "${src.kind} AudioRecord failed to initialize")
                rec?.release()
                ended()
                return@Thread
            }
            val shorts = ShortArray(RATE / 10)
            val buf = FloatArray(shorts.size)
            rec.startRecording()
            started(rec)
            try {
                while (c.run) {
                    val n = rec.read(shorts, 0, shorts.size, AudioRecord.READ_BLOCKING)
                    if (n <= 0) continue
                    for (i in 0 until n) buf[i] = shorts[i] / 32768f
                    process?.invoke(buf, n)
                    if (!src.started) place(src)
                    Native.push(src.session, buf, n)
                }
            } finally {
                rec.stop()
                rec.release()
                ended()
            }
        }, "vox-${src.kind.name.lowercase()}"))
        c.thread.start()
        return c
    }

    private fun newRecording(): String =
        java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.ROOT).format(java.util.Date()) +
            "-" + java.lang.Long.toString(System.nanoTime() % 46_656, 36)

    /** Wall clock when the open recording started. */
    @Volatile private var wallStart = 0L

    /**
     * A source's first audio: note where it starts in the recording, so
     * Me and Phone (whose capture starts later) are ordered and, on
     * playback, line up.
     */
    private fun place(src: Source) {
        src.baseMs = wallStart + elapsedMs()
        src.started = true
        val audio = src.audio ?: return
        val at = elapsedMs()
        main.post { threads.setAudioOffset(audio, at) }
    }

    /** Stop the recording (or cancel a file), then let the passes finish in the background. */
    fun stop() {
        if (file != null) {
            cancelFile = true
            return
        }
        if (!recording || finishing) return
        if (!paused) activeMs += SystemClock.elapsedRealtime() - runStart
        finishing = true
        stopCaptures()
        val srcs = sources.toList()
        val mp = projection
        projection = null
        pendingProjection = null
        ScribeService.update(app)
        // Not cancellable: each session must be finished and freed exactly once.
        scope.launch(Dispatchers.IO + NonCancellable) {
            // No capture thread may push into a session once it is finished.
            srcs.forEach { s -> s.stopping.forEach { it.thread.join() } }
            mp?.stop()
            withContext(Dispatchers.Main) { recording = false }
            val finals = srcs.map { it to Native.finish(it.session) }
            srcs.forEach(::close)
            srcs.forEach { s -> s.audio?.let { threads.setAudioLength(it, audioLength(it)) } }
            withContext(Dispatchers.Main) {
                finals.forEach { (src, json) -> applyFinal(src, json) }
                sources.clear()
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
        if (recording && !paused) sources.forEach { Native.breakParagraph(it.session) }
    }

    // ---- files ---------------------------------------------------------

    /**
     * Transcribe a shared or opened file. Returns a message for the user
     * when it can't start now.
     */
    fun transcribe(uri: Uri): String? {
        if (busy() || pendingFile != null) return "Scribe is busy; stop the current recording first."
        if (phase is Phase.NeedModels || phase is Phase.Installing) return "Download the speech models first."
        if (models == 0L) {
            pendingFile = uri
            load()
            return null
        }
        startFile(uri)
        return null
    }

    private fun startFile(uri: Uri) {
        val name = displayName(uri)
        Playback.stop()
        // Each file gets a thread of its own, named after it.
        switchTo(threads.create(name), force = true)
        val src = open(Kind.FILE, null, streaming = false, if (keepAudio) newRecording() else null)
        src.baseMs = System.currentTimeMillis()
        cancelFile = false
        fileTotalMs = 0
        file = FileJob(name, 0f)
        ScribeService.update(app)
        thread("vox-file") {
            val copy = File(app.cacheDir, "shared-audio")
            val error = runCatching {
                // A private copy: the share grant may not outlive the activity.
                app.contentResolver.openInputStream(uri)!!.use { input ->
                    copy.outputStream().use { input.copyTo(it) }
                }
                var fed = 0L
                decodeAudio(
                    copy, RATE,
                    durationMs = { fileTotalMs = it },
                    cancelled = { cancelFile },
                ) { buf, n ->
                    // Stay at most FILE_AHEAD_MS ahead of pass 2.
                    while (!cancelFile && fed - src.doneMs > FILE_AHEAD_MS) Thread.sleep(50)
                    Native.push(src.session, buf, n)
                    fed += n * 1000L / RATE
                }
            }.exceptionOrNull()
            copy.delete()
            if (error != null) Log.e(TAG, "decode failed", error)
            val json = Native.finish(src.session)
            close(src)
            src.audio?.let { threads.setAudioLength(it, audioLength(it)) }
            main.post {
                applyFinal(src, json)
                sources.remove(src)
                file = null
                lastActive = SystemClock.elapsedRealtime()
                save(now = true)
                ScribeService.update(app)
                if (error != null) {
                    android.widget.Toast.makeText(app, "Could not read $name: ${error.message}", android.widget.Toast.LENGTH_LONG).show()
                }
            }
        }
    }

    /** Duration of the file being transcribed, once known (0 if not). */
    @Volatile private var fileTotalMs = 0L

    private fun displayName(uri: Uri): String =
        runCatching {
            app.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                if (it.moveToFirst()) it.getString(0) else null
            }
        }.getOrNull() ?: uri.lastPathSegment ?: "shared audio"

    private fun thread(name: String, body: () -> Unit) = Thread(body, name).apply { start() }

    // ---- transcript ----------------------------------------------------

    private fun deleteAudio(files: List<String>) {
        files.forEach { File(audioDir, it).delete() }
    }

    fun clear() {
        if (busy()) return
        Playback.stop()
        deleteAudio(threads.clear(thread.id))
        paragraphs.clear()
        written.clear()
        refreshThreads()
    }

    // ---- threads -------------------------------------------------------

    /** Open thread [id] (only while idle, unless [force]). */
    fun switchTo(id: Long, force: Boolean = false) {
        if (busy() && !force) return
        if (id == thread.id) return
        sync()
        dropIfEmpty(thread)
        show(id)
    }

    fun newThread() {
        if (busy()) return
        sync()
        // An untouched new thread is reused rather than piling up empty ones.
        if (paragraphs.isEmpty() && isDateTitle(thread.title)) return
        dropIfEmpty(thread)
        show(threads.create())
    }

    fun rename(id: Long, title: String) {
        val t = title.trim()
        if (t.isEmpty()) return
        threads.rename(id, t)
        refreshThreads()
    }

    fun delete(id: Long) {
        if (id == thread.id && busy()) return
        if (id == thread.id) Playback.stop()
        deleteAudio(threads.delete(id))
        if (id == thread.id) {
            paragraphs.clear()
            written.clear()
            show(threads.list().firstOrNull()?.id ?: threads.create())
        } else {
            refreshThreads()
        }
    }

    private val dateTitle = Regex("""\d{4}-\d\d-\d\d \d\d:\d\d""")

    private fun isDateTitle(t: String) = dateTitle.matches(t)

    /** Forget a thread left with nothing in it and its date title. */
    private fun dropIfEmpty(t: ThreadInfo) {
        if (t.id != 0L && paragraphs.isEmpty() && isDateTitle(t.title) && threads.messages(t.id).isEmpty() &&
            threads.audioRows(t.id).isEmpty()
        ) {
            deleteAudio(threads.delete(t.id))
        }
    }

    /** Load thread [id] into [paragraphs] and remember it as the one open. */
    private fun show(id: Long) {
        Playback.stop()
        saver?.cancel()
        paragraphs.clear()
        written.clear()
        for (m in threads.messages(id)) {
            val p = Para(
                "m${m.rowId}",
                listOf(ClipUi(m.text, partial = false)),
                source = m.speaker,
                audio = m.audio,
                words = parseWords(m.words),
                atMs = m.atMs,
            )
            paragraphs += p
            written[p.id] = m
        }
        threads.setCurrent(id)
        thread = ThreadInfo(id, "", "", "", 0)
        refreshThreads()
    }

    private fun refreshThreads() {
        val list = threads.list()
        threadList.clear()
        threadList += list
        list.firstOrNull { it.id == thread.id }?.let { thread = it }
        threadAudio = if (thread.id != 0L) threads.audioRows(thread.id).associate { it.file to it.recording } else emptyMap()
    }

    /** Move the pre-threads transcript.json into a thread of its own. */
    private fun migrate() {
        val old = File(app.filesDir, "transcript.json")
        if (!old.exists()) return
        val list = runCatching { JSONArray(old.readText()) }.getOrNull()
        if (list != null && list.length() > 0) {
            val id = threads.create("Earlier transcript")
            for (i in 0 until list.length()) {
                val o = list.getJSONObject(i)
                threads.say(id, o.getString("text"), if (o.has("source")) o.getString("source") else null)
            }
            threads.setCurrent(id)
        }
        old.renameTo(File(app.filesDir, "transcript.json.migrated"))
    }

    /** The open thread as text, speakers as "Me: …". */
    fun text(): String =
        paragraphs.filter { it.text.isNotBlank() }
            .joinToString("\n\n") { p -> if (p.source != null) "${p.source}: ${p.text}" else p.text }

    /** The last finished (non-partial) text, for the notification. */
    fun lastLine(): String =
        paragraphs.asReversed().firstNotNullOfOrNull { p ->
            p.clips.filter { !it.partial && it.text.isNotBlank() }
                .takeIf { it.isNotEmpty() }
                ?.joinToString(" ") { it.text }
                ?.let { if (p.source != null) "${p.source}: $it" else it }
        } ?: ""

    private fun apply(src: Source, json: String) {
        if (src !in sources) return
        val events = JSONArray(json)
        var changed = false
        for (i in 0 until events.length()) {
            val ev = events.getJSONObject(i)
            when (ev.getString("t")) {
                "paragraph" -> {
                    val o = ev.getJSONObject("p")
                    upsert(src, parse(src, o))
                    if (ev.getString("change") != "partial") changed = true
                    if (src.kind == Kind.FILE) src.doneMs = maxOf(src.doneMs, finalEnd(o))
                }
                "removed" -> {
                    paragraphs.removeAll { it.id == ev.getString("id") }
                    changed = true
                }
                "level" -> if (!paused) {
                    src.level = ev.getDouble("rms").toFloat()
                    level = sources.maxOf { it.level }
                }
            }
        }
        if (src.kind == Kind.FILE) {
            val total = fileTotalMs
            file = file?.copy(progress = if (total > 0) (src.doneMs.toFloat() / total).coerceIn(0f, 1f) else -1f)
        }
        if (changed) save(now = false)
    }

    /** End of the last pass-2 clip in a paragraph's JSON. */
    private fun finalEnd(o: JSONObject): Long {
        val clips = o.getJSONArray("clips")
        var end = 0L
        for (i in 0 until clips.length()) {
            val c = clips.getJSONObject(i)
            if (c.getString("stage") != "partial") {
                end = maxOf(end, c.getLong("start_ms") + c.optLong("duration_ms", 0))
            }
        }
        return end
    }

    private fun applyFinal(src: Source, json: String) {
        val list = JSONObject(json).optJSONArray("paragraphs") ?: JSONArray()
        val final = (0 until list.length()).map { parse(src, list.getJSONObject(it)) }
        val keep = final.map { it.id }.toSet()
        paragraphs.removeAll { it.id in src.ids && it.id !in keep }
        final.forEach { upsert(src, it) }
    }

    /**
     * Add or replace [p], in the order it was said: a paragraph can be
     * finished after a later one (a short noise with no live text, or the
     * other source's), as the desktop's transcript also allows for.
     */
    private fun upsert(src: Source, p: Para) {
        src.ids += p.id
        val i = paragraphs.indexOfFirst { it.id == p.id }
        if (i >= 0 && paragraphs[i].atMs == p.atMs) {
            paragraphs[i] = p
            return
        }
        if (i >= 0) paragraphs.removeAt(i)
        val at = paragraphs.indexOfFirst { it.atMs > p.atMs }
        if (at < 0) paragraphs += p else paragraphs.add(at, p)
    }

    private fun parse(src: Source, o: JSONObject): Para {
        val clips = o.getJSONArray("clips")
        return Para(
            id = o.getString("id"),
            clips = (0 until clips.length()).map {
                val c = clips.getJSONObject(it)
                ClipUi(c.getString("text"), c.getString("stage") == "partial")
            },
            source = src.label,
            audio = src.audio,
            words = if (src.audio != null) parseWords(o.optJSONArray("words")) else emptyList(),
            atMs = src.baseMs + o.optLong("start_ms"),
        )
    }

    /**
     * Store the open thread's finished text (partials are left out), so
     * a killed process doesn't lose it. Debounced unless [now].
     */
    private fun save(now: Boolean) {
        saver?.cancel()
        if (now) {
            sync()
            return
        }
        saver = scope.launch {
            delay(2_000)
            sync()
        }
    }

    /** Write what changed in [paragraphs] since the last sync. */
    private fun sync() {
        saver?.cancel()
        val id = thread.id
        if (id == 0L) return
        val present = HashSet<String>()
        for (p in paragraphs) {
            val text = p.clips.filter { !it.partial && it.text.isNotBlank() }.joinToString(" ") { it.text }
            if (text.isEmpty()) continue
            present += p.id
            val words = p.words.takeIf { it.isNotEmpty() }?.toJson()
            val w = written[p.id]
            when {
                w == null -> written[p.id] =
                    Saved(threads.say(id, text, p.source, p.audio, words, p.atMs), text, p.source, p.audio, words, p.atMs)
                w.text != text || w.words != words || w.atMs != p.atMs -> {
                    threads.edit(w.rowId, text, words, p.atMs)
                    written[p.id] = w.copy(text = text, words = words, atMs = p.atMs)
                }
            }
        }
        val gone = written.keys.filter { it !in present }
        for (k in gone) written.remove(k)?.let { threads.remove(it.rowId) }
        refreshThreads()
    }

}
