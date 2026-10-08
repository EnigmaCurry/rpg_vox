package com.enigmacurry.rpg_vox_scribe

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.getValue
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

    private val main = Handler(Looper.getMainLooper())
    @Volatile private var run = false
    @Volatile private var track: AudioTrack? = null
    @Volatile private var fromMs = 0L
    private var worker: Thread? = null
    private var dir = File("")
    /** Restarts after the audio output died under us, this playback. */
    private var retries = 0

    /** Position on the playing recording's timeline (ms). */
    fun positionMs(): Long {
        val t = track ?: return fromMs
        val frames = runCatching { t.playbackHeadPosition.toLong() and 0xffffffffL }.getOrDefault(0L)
        return fromMs + frames * 1000 / PLAY_RATE
    }

    /** Play the recording made of [files] (in [dir]) from [atMs] on its timeline. */
    fun play(dir: File, files: List<AudioFile>, atMs: Long) {
        stop()
        if (files.isEmpty()) return
        val base = files.minOf { it.offsetMs }
        this.dir = dir
        retries = 0
        paused = false
        lengthMs = 0
        playing = Playing(files.associate { it.file to it.offsetMs - base })
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

    fun stop() {
        stopWorker()
        playing = null
        paused = false
    }

    private fun startWorker(from: Long) {
        val offsets = playing?.offsets ?: return
        fromMs = from
        run = true
        val me = Thread({ loop(dir, offsets, from) }, "vox-play")
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

    private class Source(val handle: Long, var silence: Long, var done: Boolean = false)

    private fun loop(dir: File, offsets: Map<String, Long>, from: Long) {
        val sources = mutableListOf<Source>()
        var t: AudioTrack? = null
        var died = false
        try {
            for ((file, off) in offsets) {
                val h = runCatching { Native.playerOpen(File(dir, file).path) }
                    .onFailure { Log.w(TAG, "can't open $file", it) }
                    .getOrNull() ?: continue
                sources += Source(h, (off - from).coerceAtLeast(0) * PLAY_RATE / 1000)
                Native.playerSeek(h, (from - off).coerceAtLeast(0))
                val end = off + Native.playerLength(h)
                main.post { if (end > lengthMs) lengthMs = end }
            }
            if (sources.isEmpty()) return
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
            while (run) {
                mix.fill(0f)
                var produced = 0
                for (s in sources) produced = maxOf(produced, add(s, mix, got))
                if (produced == 0) break
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

    /** Mix [s]'s next samples into [mix]; how many it covered (0 once it's over). */
    private fun add(s: Source, mix: FloatArray, got: FloatArray): Int {
        var i = 0
        if (s.silence > 0) {
            val k = minOf(s.silence, mix.size.toLong()).toInt()
            s.silence -= k
            i = k
        }
        if (s.done || i == mix.size) return i
        val want = mix.size - i
        val buf = if (want == got.size) got else FloatArray(want)
        val n = Native.playerRead(s.handle, buf)
        if (n < want) s.done = true
        for (j in 0 until n) mix[i + j] += buf[j]
        return i + n
    }
}
