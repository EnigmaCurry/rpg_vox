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
import androidx.compose.runtime.mutableIntStateOf
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
/** Without "Keep non-speech audio", longer quiet between words plays as this long. */
private const val MAX_GAP_MS = 1_000L
/** Louder than this (RMS dBFS) is sound, where a recording with no words starts playing. */
private const val SOUND_DB = -45f
/** Started this far before that sound. */
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
    /**
     * The reading in the language you read, for a paragraph heard in
     * another one, and the language it was heard in. Null when
     * translation is off, when the paragraph is in your own language, or
     * before the first reading of it has landed.
     */
    val translation: String? = null,
    val heardIn: String? = null,
    /**
     * The reading a sentence at a time, each with the sentence it was
     * made from, so it can be shown beside what was said rather than
     * under all of it.
     */
    val readings: List<Reading> = emptyList(),
    /**
     * What the engine says of the translation: `done`, `native` (it is
     * in your language already, so there is nothing to read), `running`,
     * `failed`, or null when it has not looked yet.
     */
    val translated: String? = null,
    /**
     * Whether a reading of this paragraph is coming at all -- whether
     * the engine that heard it had a translator. One recorded with none
     * is shown as it was said for good, whatever language it is in:
     * waiting for a reading nobody is making is how text disappears
     * behind an ellipsis.
     */
    val translatable: Boolean = false,
) {
    val text: String get() = clips.filter { it.text.isNotBlank() }.joinToString(" ") { it.text }
    /** [text]'s words as shown. */
    val shown: List<String> by lazy { text.split(' ').filter { it.isNotEmpty() } }

    /** Whether there is a reading to show, under [Show.TRANSLATION] too. */
    val reading: String? get() = translation?.takeIf { it.isNotBlank() }

    /**
     * Whether what was said is what to show, rather than its reading.
     *
     * The rule is: show it unless it is *known* to be in an alphabet the
     * reader does not read. Everything else is a reason to show it --
     * nothing translating this paragraph ([translatable]), a reading
     * that will never come, or text whose alphabet cannot be read
     * either way.
     *
     * The state that matters here is the one the engine has not answered
     * yet, and the one it answers with "nothing to translate". Every
     * paragraph starts in the first while pass 1 types it, and lands in
     * the second whenever its alphabet cannot be read -- too few
     * letters, or too much of a mixture, which is common enough with
     * `<unk>` and a stray word in it. Both used to be shown as what was
     * said, which for Russian is Russian, on a screen that is meant to
     * be English. The alphabet is asked here instead, by the same rule
     * and the same numbers the engine uses.
     *
     * Text whose alphabet cannot be read *is* shown: "OK." is as likely
     * as "Да.", and hiding every short utterance to catch the second
     * would lose the first. The engine is told to assume the language
     * pass 1 is listening in for exactly those, so they are usually read
     * rather than left for this to judge.
     */
    fun own(native: Script): Boolean {
        val foreign = script(text)?.let { it != native } == true
        return when {
            !translatable -> true
            translated == "done" -> false
            translated == "failed" -> true
            else -> !foreign
        }
    }

    /**
     * The paragraph as rows: a stretch of what was said, with the
     * reading of it when there is one.
     *
     * Built by walking [text] and taking each reading's source sentence
     * out of it in turn, so a sentence still being spoken -- or one
     * whose reading hasn't landed yet -- is a row of its own with
     * nothing under it, and the reading of each stays with the sentence
     * it is of. A reading whose sentence a later pass has rewritten is
     * dropped rather than shown against the wrong words.
     *
     * The screen shows the readings of a translated paragraph and not
     * the stretches they are of ([own] is what decides), so what this
     * answers for one of those is the order they go in and whether
     * anything said is still waiting to be read.
     */
    fun rows(): List<Row> {
        val whole = text
        if (readings.isEmpty()) return listOf(Row(whole, null))
        val out = mutableListOf<Row>()
        var pos = 0
        for (r in readings) {
            if (r.source.isEmpty()) continue
            val at = whole.indexOf(r.source, pos)
            if (at < 0) continue
            // Anything before this sentence that no reading claimed: a
            // sentence whose own reading was dropped, or the start of a
            // paragraph the translator hasn't caught up with.
            if (at > pos) {
                val before = whole.substring(pos, at).trim()
                if (before.isNotEmpty()) out += Row(before, null)
            }
            out += Row(r.source, r.text, r.startMs)
            pos = at + r.source.length
        }
        val tail = whole.substring(pos).trim()
        if (tail.isNotEmpty()) out += Row(tail, null)
        return out
    }
}

/**
 * One sentence of a reading, the sentence it was made from, and when
 * that one was spoken -- which is what lets a reading be tapped to hear
 * the words it is of, the same as tapping what was said.
 */
data class Reading(val source: String, val text: String, val startMs: Long = 0)

/**
 * The engine's `translation.lines`, and the same array as it is stored
 * beside the paragraph: `[{"source", "text", "start_ms"}, …]`. A line
 * missing either side is dropped -- a reading with no sentence to show
 * it against is no reading, and the other way round.
 */
fun parseReadings(a: JSONArray?): List<Reading> =
    (0 until (a?.length() ?: 0)).mapNotNull {
        val l = a!!.getJSONObject(it)
        val source = l.optString("source")
        val text = l.optString("text")
        when {
            source.isEmpty() || text.isEmpty() -> null
            else -> Reading(source, text, l.optLong("start_ms"))
        }
    }

fun parseReadings(json: String?): List<Reading> =
    json?.let { runCatching { parseReadings(JSONArray(it)) }.getOrNull() } ?: emptyList()

fun List<Reading>.toJson(): String =
    JSONArray(
        map { JSONObject().put("source", it.source).put("text", it.text).put("start_ms", it.startMs) },
    ).toString()

/**
 * A stretch of what was said, with the reading of it when it has one
 * and, when it has one, when that stretch was spoken.
 *
 * The screen shows the readings, and what was said only where a reading
 * has not landed yet (see `Paragraph` in MainActivity), so what this is
 * for is the order they go in, whether anything said is still waiting,
 * and where a tap on a reading plays from.
 */
data class Row(val said: String, val reading: String?, val startMs: Long = 0)

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
data class FileJob(
    val name: String,
    val progress: Float,
    /** Audio through pass 2 and the file's length (0 until known), in ms. */
    val doneMs: Long = 0,
    val totalMs: Long = 0,
    /** Started ([SystemClock.elapsedRealtime]), for the time left. */
    val startedAt: Long = SystemClock.elapsedRealtime(),
) {
    /**
     * "1:23 / 10:05 · about 2:10 left": how far it's got, and the time
     * left at the pace so far (once there's enough to go on).
     */
    fun detail(context: Context): String? {
        if (totalMs <= 0) return null
        val spent = SystemClock.elapsedRealtime() - startedAt
        val left = if (doneMs >= 5_000 && spent > 0) {
            val ms = (totalMs - doneMs).coerceAtLeast(0) * spent / doneMs
            context.getString(R.string.dock_file_left, clock(ms))
        } else {
            ""
        }
        return context.getString(R.string.dock_file_progress, clock(doneMs), clock(totalMs)) + left
    }
}

/** m:ss, or h:mm:ss from an hour. */
fun clock(ms: Long): String {
    val s = ms / 1000
    return if (s >= 3600) "%d:%02d:%02d".format(s / 3600, s / 60 % 60, s % 60) else "%d:%02d".format(s / 60, s % 60)
}

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
    lateinit var threads: ThreadStore
        private set
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
    /** With [keepAudio], also keep recordings in which no words were detected. */
    var keepNonSpeech by mutableStateOf(false)
        private set
    /** Start recording (or dictating) as soon as a new one is made with New. */
    var autoStart by mutableStateOf(false)
        private set
    /**
     * Just made with New and [autoStart] is on: the screen for it ("dictation"
     * or "transcription") starts recording once it's up, and clears this.
     */
    var startWhenOpen by mutableStateOf<String?>(null)
    /** Transcript text size, an index into [FONT_SCALES]. */
    var fontStep by mutableIntStateOf(FONT_SCALES.size / 2)
        private set
    /** The language you read in: what every translation is read into. */
    var native by mutableStateOf(Language.ENGLISH)
        private set
    /**
     * The languages listened out for and read in [native]. One per
     * alphabet, because that is what a paragraph's language is read off;
     * only those whose model is downloaded are used.
     */
    var others by mutableStateOf<Set<Language>>(emptySet())
        private set
    /**
     * Show what was said where a reading of it has not landed yet, in
     * the language it was heard in, rather than an ellipsis holding the
     * place. The reading runs a sentence behind the speaking on purpose,
     * and this is what that sentence looks like meanwhile: the words
     * pass 1 is typing, in their own language, each giving way to its
     * reading as that arrives.
     */
    var showSaid by mutableStateOf(true)
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

    /** A [Dictation] (the voice keyboard's or the app's) holds the mic and the models. */
    @Volatile var dictating = false
        private set

    /** The loaded models for a [Dictation], marking them in use; 0 if not loaded or Scribe is busy. */
    fun claimForDictation(): Long {
        if (models == 0L || busy() || dictating) return 0
        dictating = true
        lastActive = SystemClock.elapsedRealtime()
        return models
    }

    fun releaseFromDictation() {
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
        keepNonSpeech = prefs.getBoolean("keep_non_speech", false)
        autoStart = prefs.getBoolean("auto_start", false)
        fontStep = prefs.getInt("font_step", FONT_SCALES.size / 2).coerceIn(0, FONT_SCALES.size - 1)
        showSaid = prefs.getBoolean("show_said", true)
        native = Language.of(prefs.getString("native_language", null))
        others = (prefs.getStringSet("other_languages", null) ?: emptySet())
            .mapNotNull { name -> Language.entries.firstOrNull { it.name == name } }
            // A language whose model was removed, or whose alphabet now
            // clashes with a changed native language, is dropped rather
            // than failing the load it would be part of.
            .filter { it != native && store.installed(translation(it, native) ?: return@filter false) }
            .distinctBy { it.script }
            .toSet()
        // Two settings that were settings for one version each: which
        // language pass 1 listens in, which pass 1 now works out for
        // itself, and what the transcript shows of a translated
        // paragraph, which is now always the reading. The keys go rather
        // than sit there looking like something that still decides
        // anything.
        for (gone in listOf("pass1_language", "show")) {
            if (prefs.contains(gone)) prefs.edit().remove(gone).apply()
        }
        phase = if (store.ready()) Phase.Unloaded else Phase.NeedModels(resume = store.started())
        migrate()
        // A pass-1 model the settings ask for and haven't got: fetch it
        // rather than listen in the wrong language, which is what a
        // setting saved by an earlier version may be waiting for.
        if (store.ready() && pass1Missing != null) languagesChanged()
        show(threads.current())
        Dictations.init(app)
        scope.launch {
            InstallService.state.collect { s ->
                when (s) {
                    // Only the models nothing works without take over
                    // the screen; a language model is downloaded with
                    // the app still usable, and its progress is shown on
                    // the settings page that asked for it.
                    is Install.Running -> if (!store.ready()) {
                        phase = Phase.Installing(s.step, s.overall)
                    }
                    is Install.Failed ->
                        if (phase is Phase.Installing) phase = Phase.NeedModels(s.error, resume = true)
                    Install.Done -> {
                        languageModels++
                        if (phase is Phase.Installing) load() else reload()
                    }
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

    fun chooseKeepNonSpeech(keep: Boolean) {
        keepNonSpeech = keep
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putBoolean("keep_non_speech", keep).apply()
    }

    fun chooseAutoStart(on: Boolean) {
        autoStart = on
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putBoolean("auto_start", on).apply()
    }

    fun chooseShowSaid(on: Boolean) {
        showSaid = on
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putBoolean("show_said", on).apply()
    }

    fun chooseFontStep(step: Int) {
        fontStep = step.coerceIn(0, FONT_SCALES.size - 1)
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit().putInt("font_step", fontStep).apply()
    }

    /**
     * The language you read in. Pass 1 listens in it, so this decides
     * which streaming model loads, and every translation reads into it,
     * so the other languages are dropped: their models read into the old
     * one, and a language sharing the new one's alphabet could not be
     * told apart from it anyway.
     */
    fun chooseNative(l: Language) {
        if (l == native) return
        native = l
        others = emptySet()
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit()
            .putString("native_language", l.label)
            .putStringSet("other_languages", emptySet())
            .apply()
        languagesChanged()
    }

    /**
     * After any language change: fetch the pass-1 models the languages
     * now need, and load everything again.
     *
     * Those are fetched rather than offered. Each is 27 MB, and a
     * language chosen here is a language the live text has to be able to
     * be in -- it is not a choice to put behind a button, which is how
     * this went wrong before. The translation models are 81 MB each and
     * stay behind the Download button.
     */
    private fun languagesChanged() {
        val missing = pass1Models().map { it.second }.filterNot(store::installed)
        if (missing.isNotEmpty() && store.ready()) InstallService.start(app, missing)
        reload()
    }

    /**
     * Pass 1's models, as `language to model`, the first being the
     * language it starts in.
     *
     * Pass 1 shows words as they are spoken and so has to be told which
     * language to listen in, and nobody is asked: it starts in the
     * language you read in and moves to another of these when an
     * utterance turns out to have been in it, which the engine does by
     * the alphabet pass 2 heard (`Passes::streaming_also`). English is
     * always among them -- it is downloaded anyway, and a sentence of it
     * in a Russian conversation is the common case of this.
     */
    fun pass1Models(): List<Pair<Language, Model>> =
        (listOf(native) + others + Language.ENGLISH)
            .distinct()
            .mapNotNull { l -> l.streaming?.let { l to it } }

    /**
     * Listen out for [l] as well, or stop. Turning one on replaces
     * whichever other language shares its alphabet, since a paragraph in
     * either could not be told from one in the other.
     */
    fun chooseOther(l: Language, on: Boolean) {
        val next = when {
            !on -> others - l
            else -> others.filter { it.script != l.script }.toSet() + l
        }
        if (next == others) return
        others = next
        app.getSharedPreferences("scribe", Context.MODE_PRIVATE).edit()
            .putStringSet("other_languages", next.map { it.name }.toSet())
            .apply()
        languagesChanged()
    }

    /**
     * Whether anything is being translated: the languages the loaded
     * models were actually given, not the ones the settings ask for.
     *
     * The difference is what the screen hangs on. A setting changed
     * while a recording is running cannot be applied until the engine
     * is loaded again, and until then the screen must show what the
     * engine can do, not what the settings say it should: believing the
     * settings means hiding a language nothing is translating, which is
     * text that never appears at all.
     */
    val translating: Boolean get() = loadedOthers.isNotEmpty()

    /**
     * Finished paragraphs still waiting to be read, which is what a
     * reading falling behind the speaking looks like.
     *
     * The paragraph being spoken is never one of them: a reading runs a
     * sentence behind on purpose, and the last paragraph always has
     * something in it nobody has read yet. Two or more finished ones
     * waiting is the phone not keeping up -- which happens when Scribe
     * is not the app on screen, where Android gives it the slow cores
     * and the translator is the first thing to run short.
     */
    val readingBehind: Int
        get() = paragraphs.dropLast(1).count { p ->
            !saidAsIs(p) && p.rows().any { it.reading == null }
        }

    /** The other languages the loaded models were given; see [translating]. */
    @Volatile
    private var loadedOthers: List<Language> = emptyList()

    /**
     * A language change that had to wait: the models can only be swapped
     * between recordings, and the one in progress keeps the engine it
     * started with.
     */
    @Volatile
    private var reloadWanted = false

    /** Whether [p] is shown as what was said, rather than as its reading. */
    fun saidAsIs(p: Para) = p.own(native.script)

    /** The models the languages now ask for, in the order they install. */
    fun wanted(): List<Model> =
        pass1Models().map { it.second } + others.mapNotNull { translation(it, native) }

    /**
     * A language pass 1 should be able to listen in and has no model for
     * yet, or null when there is none. Until it arrives the live text
     * cannot be in that language, and saying so is the point of this: a
     * wrong language in the live text with no explanation is what this
     * used to produce without a word.
     */
    val pass1Missing: Language?
        get() = pass1Models().firstOrNull { !store.installed(it.second) }?.first

    /** Models downloaded for a language setting that no longer wants them. */
    fun spare(): List<Model> {
        val keep = (wanted() + BUNDLES).map { it.dir }.toSet()
        return (NATIVE_LANGUAGES.mapNotNull { it.streaming } +
            NATIVE_LANGUAGES.flatMap { into -> translatable(into).mapNotNull { translation(it, into) } })
            .filter { it.dir !in keep && store.installed(it) }
            .distinctBy { it.dir }
    }

    /**
     * Load the models again, for a language setting that changed. A
     * recording in progress keeps the engine it started with; the next
     * one gets the new one.
     *
     * Nothing is unloaded until everything the new setting wants is
     * downloaded: a language turned on before its model is fetched would
     * otherwise leave the app with no models at all until it arrived.
     * The download calls back here when it lands.
     */
    private fun reload() {
        if (busy() || dictating || !store.ready() || !wanted().all(store::installed)) {
            // Not now: when the recording ends, or when the model it is
            // waiting for arrives. Forgetting it here is how a setting
            // changed mid-recording stayed unapplied for good.
            reloadWanted = true
            return
        }
        reloadWanted = false
        release()
        if (phase == Phase.Unloaded) load()
    }

    /** Apply a language change that had to wait for the engine to be free. */
    private fun reloadIfWanted() {
        if (reloadWanted) reload()
    }

    /** Where kept recordings live. */
    val audioDir: File get() = File(app.filesDir, "audio")

    /**
     * The open thread's recordings end to end, oldest first, as one
     * timeline, each at its full length: each file with where it plays
     * on it, and where to start each recording from. That's its start,
     * except for a recording with no words, which starts at its first
     * sound. Fills in lengths and sound spans not known yet.
     */
    private fun timeline(): Pair<List<AudioFile>, Map<String, Long>> {
        val files = mutableListOf<AudioFile>()
        val starts = HashMap<String, Long>()
        val heard = paragraphs.mapNotNullTo(HashSet()) { it.audio }
        var at = 0L
        for ((rec, rows) in threads.audioRows(thread.id).groupBy { it.recording }) {
            // Without "Keep non-speech audio", a recording with no text left isn't played.
            if (!keepNonSpeech && rows.none { it.file in heard }) continue
            val base = rows.minOf { it.offsetMs }
            var end = 0L
            var sound = Long.MAX_VALUE
            for (r in rows) {
                val off = r.offsetMs - base
                val len = r.lengthMs.takeIf { it > 0 } ?: audioLength(r.file).also { threads.setAudioLength(r.file, it) }
                files += AudioFile(r.file, at + off)
                end = maxOf(end, off + len)
                if (rows.none { it.file in heard }) {
                    val start = if (r.soundStartMs >= 0) {
                        r.soundStartMs.takeIf { r.soundEndMs > it }
                    } else {
                        val span = audioSound(r.file)
                        threads.setAudioSound(r.file, span?.first ?: 0, span?.second ?: 0)
                        span?.first
                    }
                    if (start != null) sound = minOf(sound, off + start)
                }
            }
            starts[rec] = at + if (sound == Long.MAX_VALUE) 0 else sound
            at += end
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

    /**
     * What playback jumps over on the timeline of [files]: the open
     * thread's cut stretches, and without "Keep non-speech audio", any
     * quiet between words longer than [MAX_GAP_MS] (trimmed to that).
     */
    private fun skips(files: List<AudioFile>): List<LongRange> {
        val cuts = threads.cuts(thread.id)
        val out = files.flatMap { f -> cuts[f.file].orEmpty().map { (f.offsetMs + it.first)..(f.offsetMs + it.last) } }
            .toMutableList()
        if (!keepNonSpeech) {
            val offsets = files.associate { it.file to it.offsetMs }
            val speech = paragraphs.flatMap { p ->
                val off = offsets[p.audio ?: return@flatMap emptyList()] ?: return@flatMap emptyList()
                p.words.map { (off + it.startMs) to (off + it.endMs) }
            }.sortedBy { it.first }
            val half = MAX_GAP_MS / 2
            var heard = 0L - half // the start counts as speech, less half a gap
            for ((a, b) in speech) {
                if (a - heard > MAX_GAP_MS) out += (heard + half)..(a - half)
                heard = maxOf(heard, b)
            }
            // After the last word, nothing left to hear.
            if (speech.isNotEmpty()) out += (heard + half)..(Long.MAX_VALUE / 4)
        }
        return out
    }

    /** Play the open thread's audio, every recording in turn, from the start of [recording]. */
    fun playFromStart(recording: String) {
        if (busy()) return
        val (files, starts) = timeline()
        Playback.play(audioDir, files, starts[recording] ?: return, skips(files))
        PlaybackService.start(app)
    }

    /** Play the open thread's audio, every recording in turn, from [word] of [p]. */
    fun play(p: Para, word: Word) {
        if (busy()) return
        val (files, _) = timeline()
        val offset = files.firstOrNull { it.file == p.audio }?.offsetMs ?: return
        Playback.play(audioDir, files, offset + word.startMs, skips(files))
        PlaybackService.start(app)
    }

    /**
     * Cut characters [from] until [to] of [p]'s text: the words they touch
     * go from the transcript, and with kept audio, their stretch of it is
     * skipped on playback (the file stays whole).
     */
    fun cut(p: Para, from: Int, to: Int) {
        Playback.stop()
        val shown = p.shown
        var at = 0
        val gone = mutableListOf<Int>()
        shown.forEachIndexed { i, w ->
            if (at < to && at + w.length > from) gone += i
            at += w.length + 1
        }
        if (gone.isEmpty()) return
        val keep = shown.filterIndexed { i, _ -> i !in gone }
        val text = keep.joinToString(" ")
        var words = emptyList<Word>()
        if (p.words.isNotEmpty()) {
            val a = p.wordAt(gone.first(), shown.size).startMs
            val b = p.wordAt(gone.last(), shown.size).endMs
            p.audio?.let { if (b > a) threads.addCut(it, a, b) }
            // Still being transcribed: the engine's later versions of this
            // paragraph are cut the same way (see [parse]).
            sources.firstOrNull { p.id in it.ids }?.cuts?.add(a..b)
            words = if (p.words.size == shown.size) {
                p.words.filterIndexed { i, _ -> i !in gone }
            } else {
                realign(p.words, text)
            }
        }
        replace(p, text, words)
    }

    /** Replace [p]'s text with [text]; word times carry over where the words match. */
    fun edit(p: Para, text: String) {
        if (busy()) return
        val t = text.split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(" ")
        if (t == p.text) return
        Playback.stop()
        replace(p, t, if (p.words.isEmpty() || t.isEmpty()) emptyList() else realign(p.words, t))
    }

    private fun realign(words: List<Word>, text: String): List<Word> =
        runCatching { parseWords(Native.alignWords(words.toJson(), text, words.first().startMs, words.last().endMs)) }
            .getOrElse { emptyList() }

    /** [p] with new text and word times, or gone if [text] is empty; then stored. */
    private fun replace(p: Para, text: String, words: List<Word>) {
        val i = paragraphs.indexOfFirst { it.id == p.id }
        if (i < 0) return
        if (text.isEmpty()) {
            paragraphs.removeAt(i)
        } else {
            paragraphs[i] = p.copy(clips = listOf(ClipUi(text, partial = false)), words = words)
        }
        sync()
        // Its recording left without text: gone too, unless non-speech audio is
        // kept. (While recording, the end of the recording decides that.)
        val rec = p.audio?.let { threadAudio[it] }
        if (rec != null && !keepNonSpeech && !busy()) {
            val files = threadAudio.filterValues { it == rec }.keys
            if (paragraphs.none { it.audio in files }) {
                files.forEach(threads::removeAudio)
                deleteAudio(files.toList())
                refreshThreads()
            }
        }
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
        if (!v) return
        reloadIfWanted()
        if (phase == Phase.Unloaded) load()
    }

    /**
     * The first run's download: the two models nothing works without,
     * plus whatever the language settings already ask for -- the pass-1
     * model for the language chosen is one of those, and a first run in
     * Russian that fetched only the English one would leave the live
     * text in the wrong language.
     */
    fun install() = InstallService.start(app, (BUNDLES + wanted()).distinctBy { it.dir })

    /**
     * Download the models the languages ask for and nothing else: the
     * pass-1 model for the language you read in, and one translation
     * model per other language. Already-installed ones are skipped, so
     * this is also how a failed one is retried.
     */
    fun installLanguages() {
        val missing = wanted().filterNot(store::installed)
        if (missing.isEmpty()) {
            // Nothing to fetch, but the setting may still be waiting for
            // the engine to be rebuilt around it.
            reload()
            return
        }
        InstallService.start(app, missing)
    }

    /** Delete a model a language setting no longer wants. */
    fun removeModel(m: Model) {
        store.remove(m)
        languageModels++
    }

    /**
     * Bumped whenever a model is installed or removed, so the settings
     * page is recomposed: what is on disk is not state Compose watches.
     */
    var languageModels by mutableIntStateOf(0)
        private set

    fun load() {
        if (phase == Phase.Loading || models != 0L) return
        phase = Phase.Loading
        scope.launch {
            // The languages the engine is about to be given, which is
            // what the screen goes by (see [translating]).
            val langs = others.filter { l -> translation(l, native)?.let(store::installed) == true }
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    Native.load(
                        store.dir.path,
                        THREADS,
                        // `language=directory` per pass-1 model, the
                        // first being the one it starts in. Only the
                        // ones that are here: a language still
                        // downloading cannot be listened in, and
                        // [pass1Missing] is what says so.
                        pass1Models()
                            .filter { store.installed(it.second) }
                            .ifEmpty { listOf(Language.ENGLISH to ZIPFORMER_EN) }
                            .joinToString(",") { (l, m) -> "${l.label}=${m.dir}" },
                        native.label,
                        // Only the ones whose model is actually there:
                        // the engine refuses a language it cannot read.
                        langs.joinToString(",") { it.label },
                    )
                }
            }
            result.onSuccess {
                models = it
                loadedOthers = langs
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
        loadedOthers = emptyList()
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
        /** Cut stretches (session ms): their words are left out of this session's paragraphs. */
        val cuts = mutableListOf<LongRange>()
        /** Epoch ms at the session's time zero, for ordering its paragraphs. */
        @Volatile var baseMs = 0L
        /** Paragraphs this session produced (to apply its final transcript). */
        val ids = mutableSetOf<String>()
        var capture: Capture? = null
        val stopping = mutableListOf<Capture>()
        @Volatile var polling = true
        /** Set while the main thread still has a batch of this source's events. */
        @Volatile var applying = false
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
                // Nothing is drained while the main thread is still on
                // the batch before: the events wait in the engine's
                // queue, where the next drain collapses each paragraph
                // to its latest state, rather than piling up as strings
                // the main thread is never going to catch up with.
                if (src.applying) {
                    Thread.sleep(10)
                    continue
                }
                val events = Native.poll(src.session, 100)
                if (events.length > 2) {
                    src.applying = true
                    main.post {
                        try {
                            apply(src, events)
                        } finally {
                            src.applying = false
                        }
                    }
                }
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
        // A thread made minutes ago is named for this press, not for the
        // wait before it: its date gains the time now. Only while the
        // title is still the app's and nothing has been heard in it yet
        // (kept audio counts, even when no words came of it).
        if (thread.autoTitle && paragraphs.isEmpty() && threadAudio.isEmpty()) {
            threads.stampTitle(thread.id)
            refreshThreads()
        }
        recordingMode = m
        sources.clear()
        val rec = if (keepAudio) newRecording() else null
        if (m.mic) open(Kind.MIC, if (m.phone) SPEAKER_ME else null, streaming = true, rec)
        if (m.phone) open(Kind.PHONE, SPEAKER_PHONE, streaming = true, rec)
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
                rec.addOnRoutingChangedListener({ r -> main.post { micName = (r as AudioRecord).inputName(app) } }, main)
                main.post { micName = rec.inputName(app) }
                Log.i(
                    TAG,
                    "mic: ${rec.routedDevice?.productName ?: "built-in"}, boost ${boost.name}, " +
                        "platform AGC available: ${AutomaticGainControl.isAvailable()}",
                )
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
                if (!keepNonSpeech) dropIfWordless(srcs)
                ScribeService.update(app)
                // A language changed while this was running is applied
                // now, which is what "the next recording" means.
                reloadIfWanted()
            }
        }
    }

    // ---- files ---------------------------------------------------------

    /**
     * Transcribe a shared or opened file. Returns a message for the user
     * when it can't start now.
     */
    fun transcribe(uri: Uri): String? {
        if (busy() || pendingFile != null) return app.getString(R.string.busy_stop_recording)
        if (phase is Phase.NeedModels || phase is Phase.Installing) return app.getString(R.string.need_models)
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
                if (!keepNonSpeech) dropIfWordless(listOf(src))
                ScribeService.update(app)
                if (error != null) {
                    android.widget.Toast.makeText(
                        app,
                        app.getString(R.string.file_unreadable, name, error.message),
                        android.widget.Toast.LENGTH_LONG,
                    ).show()
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
        }.getOrNull() ?: uri.lastPathSegment ?: app.getString(R.string.shared_audio)

    private fun thread(name: String, body: () -> Unit) = Thread(body, name).apply { start() }

    // ---- transcript ----------------------------------------------------

    /**
     * A finished recording's audio, when no paragraph has words in it
     * ("Keep non-speech audio" off). With two sources, the Phone audio
     * stays when only Me said anything, since they play together.
     */
    private fun dropIfWordless(srcs: List<Source>) {
        val files = srcs.mapNotNull { it.audio }.toSet()
        if (files.isEmpty()) return
        if (paragraphs.any { it.audio in files && it.words.isNotEmpty() }) return
        files.forEach(threads::removeAudio)
        deleteAudio(files.toList())
        refreshThreads()
    }

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

    /** Delete one recording of the open thread: its audio and the text heard in it. */
    fun deleteRecording(recording: String) {
        if (busy()) return
        Playback.stop()
        val files = threadAudio.filterValues { it == recording }.keys
        val gone = paragraphs.filter { it.audio in files }
        for (p in gone) written.remove(p.id)?.let { threads.remove(it.rowId) }
        paragraphs.removeAll(gone)
        files.forEach(threads::removeAudio)
        deleteAudio(files.toList())
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
        if (paragraphs.isEmpty() && thread.autoTitle) return
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

    /** Forget a thread left with nothing in it and a title nobody chose. */
    private fun dropIfEmpty(t: ThreadInfo) {
        if (t.id != 0L && paragraphs.isEmpty() && t.autoTitle && threads.messages(t.id).isEmpty() &&
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
                translation = m.translation,
                heardIn = m.heardIn,
                // A stored paragraph keeps its reading in one piece: the
                // sentences it was made of are not stored, and a
                // finished paragraph has no tail to be waiting for, so
                // one row is all the screen needs of it. Without this a
                // reopened thread would show the language it was spoken
                // in, which is the one thing the screen never shows.
                // Each sentence with the one it was made from, as it
                // was heard: what the screen shows a reading against,
                // and where a tap on it plays from. A row stored before
                // there was a column for them keeps the whole reading
                // against the whole paragraph, which is all it has.
                readings = parseReadings(m.lines).ifEmpty {
                    m.translation
                        ?.takeIf { it.isNotBlank() }
                        ?.let { listOf(Reading(m.text, it)) }
                        ?: emptyList()
                },
                translated = if (m.translation.isNullOrBlank()) null else "done",
                // A stored paragraph with no reading is never getting
                // one: nothing is listening to it any more.
                translatable = !m.translation.isNullOrBlank(),
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
            val id = threads.create(app.getString(R.string.earlier_transcript))
            for (i in 0 until list.length()) {
                val o = list.getJSONObject(i)
                threads.say(id, o.getString("text"), if (o.has("source")) o.getString("source") else null)
            }
            threads.setCurrent(id)
        }
        old.renameTo(File(app.filesDir, "transcript.json.migrated"))
    }

    /** The open thread as text, speakers as "Me: …". */
    /**
     * The transcript as the screen shows it, for Copy and Share: the
     * reading of a paragraph that has one, and what was said when it is
     * in the language you read. Copying the other language would be
     * copying text the person doing it cannot read.
     */
    fun text(): String =
        paragraphs.mapNotNull { p ->
            val body = when {
                saidAsIs(p) -> p.text
                else -> p.readings.joinToString(" ") { it.text }
            }
            body.takeIf { it.isNotBlank() }
                ?.let { if (p.source != null) "${speakerLabel(app, p.source)}: $it" else it }
        }.joinToString("\n\n")

    /**
     * The last finished (non-partial) text, for the notification: the
     * reading of a paragraph that has one, as on the screen.
     */
    fun lastLine(): String =
        paragraphs.asReversed().firstNotNullOfOrNull { p ->
            when {
                !saidAsIs(p) -> p.readings.lastOrNull()?.text
                else -> p.clips.filter { !it.partial && it.text.isNotBlank() }
                    .takeIf { it.isNotEmpty() }
                    ?.joinToString(" ") { it.text }
            }
                ?.let { if (p.source != null) "${speakerLabel(app, p.source)}: $it" else it }
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
            file = file?.copy(
                progress = if (total > 0) (src.doneMs.toFloat() / total).coerceIn(0f, 1f) else -1f,
                doneMs = minOf(src.doneMs, total),
                totalMs = total,
            )
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
        if (p.text.isEmpty() && p.clips.none { it.partial }) {
            // All of it cut.
            if (i >= 0) paragraphs.removeAt(i)
            return
        }
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
        // `{"state": "done", "language": …, "text": …}` once a reading
        // has landed; "running", "native" and "failed" have none to show,
        // and a paragraph in your own language stays at "native".
        val state = o.optJSONObject("translation")
        val tr = state?.takeIf { it.optString("state") == "done" }
        val lines = tr?.optJSONArray("lines")
        val p = Para(
            id = o.getString("id"),
            clips = (0 until clips.length()).map {
                val c = clips.getJSONObject(it)
                ClipUi(c.getString("text"), c.getString("stage") == "partial")
            },
            source = src.label,
            audio = src.audio,
            words = parseWords(o.optJSONArray("words")),
            atMs = src.baseMs + o.optLong("start_ms"),
            translation = tr?.optString("text")?.takeIf { it.isNotEmpty() },
            heardIn = tr?.optString("language")?.takeIf { it.isNotEmpty() },
            readings = parseReadings(lines),
            translated = state?.optString("state")?.takeIf { it.isNotEmpty() },
            // Whether this engine has a translator at all, asked now
            // rather than when the paragraph is drawn: the answer
            // changes between recordings, and a paragraph heard without
            // one never gets a reading however the settings end up.
            translatable = translating,
        )
        return withoutCuts(src, p)
    }

    /** [p] without the words [src] has had cut (finished text only). */
    private fun withoutCuts(src: Source, p: Para): Para {
        if (src.cuts.isEmpty() || p.clips.any { it.partial }) return p
        val shown = p.shown
        if (p.words.size != shown.size) return p
        val keep = p.words.indices.filter { i -> src.cuts.none { p.words[i].startMs in it } }
        if (keep.size == shown.size) return p
        val text = keep.joinToString(" ") { shown[it] }
        return p.copy(clips = listOf(ClipUi(text, partial = false)), words = keep.map { p.words[it] })
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
            val lines = p.readings.takeIf { it.isNotEmpty() }?.toJson()
            val w = written[p.id]
            when {
                w == null -> written[p.id] = Saved(
                    threads.say(id, text, p.source, p.audio, words, p.atMs, p.translation, p.heardIn, lines),
                    text, p.source, p.audio, words, p.atMs, p.translation, p.heardIn, lines,
                )
                w.text != text || w.words != words || w.atMs != p.atMs ||
                    w.translation != p.translation || w.lines != lines -> {
                    threads.edit(w.rowId, text, words, p.atMs, p.translation, p.heardIn, lines)
                    written[p.id] = w.copy(
                        text = text, words = words, atMs = p.atMs,
                        translation = p.translation, heardIn = p.heardIn, lines = lines,
                    )
                }
            }
        }
        val gone = written.keys.filter { it !in present }
        for (k in gone) written.remove(k)?.let { threads.remove(it.rowId) }
        refreshThreads()
    }

}
