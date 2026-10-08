package com.enigmacurry.rpg_vox_scribe

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.io.File
import org.json.JSONArray
import org.json.JSONObject

private const val TAG = "Playback"
/** Opus decodes at 48 kHz. */
private const val PLAY_RATE = 48_000

/** A word and its span in its recording, in ms. */
data class Word(val text: String, val startMs: Long, val endMs: Long)

/** The engine's `words` array: `[{"text", "start_ms", "end_ms"}, …]`. */
fun parseWords(a: JSONArray?): List<Word> =
    if (a == null) {
        emptyList()
    } else {
        (0 until a.length()).map {
            val w = a.getJSONObject(it)
            Word(w.getString("text"), w.getLong("start_ms"), w.getLong("end_ms"))
        }
    }

fun parseWords(json: String?): List<Word> =
    json?.let { runCatching { parseWords(JSONArray(it)) }.getOrNull() } ?: emptyList()

fun List<Word>.toJson(): String =
    JSONArray(map { JSONObject().put("text", it.text).put("start_ms", it.startMs).put("end_ms", it.endMs) }).toString()

/**
 * What is playing: the files of one recording, each with its start on
 * the shared timeline (ms, the earliest at 0).
 */
data class Playing(val offsets: Map<String, Long>)

/**
 * Plays a kept recording from a point, all of its sources (Me and Phone)
 * mixed, through one [AudioTrack]. [positionMs] is where playback is on
 * the recording's timeline, for the karaoke highlight.
 */
object Playback {
    var playing by mutableStateOf<Playing?>(null)
        private set
    /** Playing, but held where it is. */
    var paused by mutableStateOf(false)
        private set
    /** Length of the playing recording (ms), once known. */
    var lengthMs by mutableStateOf(0L)
        private set
    /** Counts jumps in position (seeks, skips), for anything mirroring it. */
    var seeks by mutableIntStateOf(0)
        private set

    private val main = Handler(Looper.getMainLooper())
    @Volatile private var run = false
    @Volatile private var track: AudioTrack? = null
    @Volatile private var fromMs = 0L
    /**
     * Where playback jumped over a cut, as (frames written before it,
     * timeline ms after it), so [positionMs] follows the jumps.
     */
    @Volatile private var jumps = emptyList<Pair<Long, Long>>()
    private var worker: Thread? = null
    private var dir = File("")
    private var files = emptyList<AudioFile>()
    /** Stretches of the timeline skipped over (cut text), sorted. */
    private var skips = emptyList<LongRange>()
    /** Restarts after the audio output died under us, this playback. */
    private var retries = 0

    /** Position on the playing recording's timeline (ms). */
    fun positionMs(): Long {
        val t = track ?: return fromMs
        val frames = runCatching { t.playbackHeadPosition.toLong() and 0xffffffffL }.getOrDefault(0L)
        val (at, ms) = jumps.lastOrNull { it.first <= frames } ?: (0L to fromMs)
        return ms + (frames - at) * 1000 / PLAY_RATE
    }

    /**
     * Play [files] (in [dir]), placed on one timeline, from [atMs] on it,
     * jumping over [skips].
     */
    fun play(dir: File, files: List<AudioFile>, atMs: Long, skips: List<LongRange> = emptyList()) {
        stop()
        if (files.isEmpty()) return
        this.dir = dir
        this.files = files
        this.skips = skips.sortedBy { it.first }
        retries = 0
        paused = false
        lengthMs = 0
        playing = Playing(files.associate { it.file to it.offsetMs })
        startWorker(atMs.coerceAtLeast(0))
    }

    /**
     * Pausing lets go of the audio output and resuming opens a new one at
     * the same spot: an output left paused can be torn down meanwhile (a
     * Bluetooth headset idling), and wouldn't play again.
     */
    fun pause() {
        if (playing == null || paused) return
        val at = positionMs()
        stopWorker()
        fromMs = at
        paused = true
    }

    fun resume() {
        if (playing == null || !paused) return
        paused = false
        startWorker(fromMs)
    }

    /** Jump [deltaMs] back or ahead, staying paused if paused. */
    fun skip(deltaMs: Long) = seekTo(positionMs() + deltaMs)

    /** Jump to [ms] on the timeline, staying paused if paused. */
    fun seekTo(ms: Long) {
        if (playing == null) return
        val end = lengthMs.takeIf { it > 0 } ?: Long.MAX_VALUE
        val at = ms.coerceIn(0, end)
        seeks++
        if (paused) {
            fromMs = at
        } else {
            stopWorker()
            startWorker(at)
        }
    }

    fun stop() {
        stopWorker()
        playing = null
        paused = false
    }

    private fun startWorker(from: Long) {
        if (playing == null) return
        val files = files
        val skips = skips
        fromMs = from
        jumps = emptyList()
        run = true
        val me = Thread({ loop(dir, files, skips, from) }, "vox-play")
        worker = me
        me.start()
    }

    private fun stopWorker() {
        run = false
        // A blocked write returns once what's queued is dropped.
        track?.runCatching {
            pause()
            flush()
        }
        worker?.join()
        worker = null
    }

    /**
     * One file on the timeline, from [startMs] to [endMs]; once [place]d,
     * [silence] to play before its audio, then at most [left] samples of it.
     */
    private class Source(val handle: Long, val startMs: Long, val endMs: Long) {
        var silence = 0L
        var left = 0L
        var done = false

        /** Ready to play from [at] on the timeline. */
        fun place(at: Long) {
            val begin = maxOf(at, startMs)
            done = endMs <= begin
            silence = if (done) 0 else (begin - at) * PLAY_RATE / 1000
            left = if (done) 0 else (endMs - begin) * PLAY_RATE / 1000
            if (!done) Native.playerSeek(handle, begin - startMs)
        }
    }

    private fun loop(dir: File, files: List<AudioFile>, skips: List<LongRange>, from: Long) {
        val sources = mutableListOf<Source>()
        var t: AudioTrack? = null
        var died = false
        try {
            for (f in files) {
                val off = f.offsetMs
                val h = runCatching { Native.playerOpen(File(dir, f.file).path) }
                    .onFailure { Log.w(TAG, "can't open ${f.file}", it) }
                    .getOrNull() ?: continue
                val end = minOf(f.untilMs, off + Native.playerLength(h))
                main.post { if (end > lengthMs) lengthMs = end }
                sources += Source(h, off, end)
            }
            if (sources.isEmpty()) return
            // Starting inside a cut starts after it.
            var pos = skips.firstOrNull { from in it }?.let { it.last } ?: from
            if (pos != from) fromMs = pos
            sources.forEach { it.place(pos) }
            val min = AudioTrack.getMinBufferSize(PLAY_RATE, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_FLOAT)
            t = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                        .setSampleRate(PLAY_RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .build(),
                )
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(maxOf(min, PLAY_RATE / 5 * 4)) // ~200 ms
                .build()
            track = t
            t.play()
            val mix = FloatArray(PLAY_RATE / 20) // 50 ms
            val got = FloatArray(mix.size)
            var written = 0L
            // Timeline ms of the next sample, in samples (exact).
            var posSamples = pos * PLAY_RATE / 1000
            while (run) {
                pos = posSamples * 1000 / PLAY_RATE
                // At a cut: jump past it.
                val cut = skips.firstOrNull { pos in it && it.last > pos }
                if (cut != null) {
                    sources.forEach { it.place(cut.last) }
                    posSamples = cut.last * PLAY_RATE / 1000
                    jumps = jumps + (written to cut.last)
                    continue
                }
                // Stop short of the next cut.
                val next = skips.firstOrNull { it.first > pos }?.first
                val room = if (next == null) mix.size else
                    minOf(mix.size.toLong(), maxOf(1L, next * PLAY_RATE / 1000 - posSamples)).toInt()
                mix.fill(0f)
                var produced = 0
                for (s in sources) produced = maxOf(produced, add(s, mix, got, room))
                if (produced == 0) break
                posSamples += produced
                for (i in 0 until produced) mix[i] = mix[i].coerceIn(-1f, 1f)
                val n = t.write(mix, 0, produced, AudioTrack.WRITE_BLOCKING)
                if (n < 0) {
                    // The output went away (e.g. a route change); pick up
                    // where it got to on a new one.
                    if (run) died = true
                    break
                }
                written += produced
            }
            // Let the buffered tail play out.
            while (run && !died && (t.playbackHeadPosition.toLong() and 0xffffffffL) < written) {
                if (t.playState != AudioTrack.PLAYSTATE_PLAYING) {
                    died = true
                    break
                }
                Thread.sleep(20)
            }
        } catch (e: Exception) {
            Log.e(TAG, "playback failed", e)
        } finally {
            val at = positionMs()
            track = null
            t?.runCatching {
                pause()
                flush()
                release()
            }
            sources.forEach { Native.playerFree(it.handle) }
            val me = Thread.currentThread()
            // Ended by itself: clear the state, unless a newer playback took over.
            main.post {
                if (worker !== me) return@post
                worker = null
                if (died && retries < 3) {
                    retries++
                    Log.w(TAG, "audio output died at $at ms; reopening")
                    startWorker(at)
                } else {
                    playing = null
                    paused = false
                }
            }
        }
    }

    /** Mix up to [room] of [s]'s next samples into [mix]; how many it covered (0 once it's over). */
    private fun add(s: Source, mix: FloatArray, got: FloatArray, room: Int): Int {
        var i = 0
        if (s.silence > 0) {
            val k = minOf(s.silence, room.toLong()).toInt()
            s.silence -= k
            i = k
        }
        if (s.done || i == room) return i
        val want = minOf((room - i).toLong(), s.left).toInt()
        if (want == 0) {
            s.done = true
            return i
        }
        val buf = if (want == got.size) got else FloatArray(want)
        val n = Native.playerRead(s.handle, buf)
        s.left -= n
        if (n < want || s.left == 0L) s.done = true
        for (j in 0 until n) mix[i + j] += buf[j]
        return i + n
    }
}
