package com.enigmacurry.rpg_vox_scribe

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Handler
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject

private const val TAG = "Dictation"
private const val RATE = 16_000

/**
 * One dictation, the voice keyboard's or the app's: an engine session
 * fed by the mic, on models from [Scribe.claimForDictation]. Events
 * arrive on [main] through [onChange]; [finish] or [cancel] ends it,
 * frees the session and gives the models back.
 */
class Dictation(
    private val models: Long,
    private val am: AudioManager,
    private val boost: Boost,
    private val route: MicRoute,
    private val main: Handler,
    private val onChange: (List<KPara>) -> Unit,
    /** The mic's level (RMS), every 50 ms or so. */
    private val onLevel: (Float) -> Unit = {},
) {
    private var session = 0L
    @Volatile private var capturing = true
    @Volatile private var polling = true
    @Volatile private var ended = false
    /** The mic keeps running but its audio is dropped, so resuming is instant. */
    @Volatile var paused = false
    /** Samples pushed so far: the session's audio clock. */
    @Volatile private var pushed = 0L
    @Volatile private var breakAsked = false
    private var capture: Thread? = null
    private var poller: Thread? = null
    private val paras = LinkedHashMap<String, KPara>()

    fun start() {
        session = Native.start(models, RATE, true, null)
        poller = Thread({
            while (polling) {
                val events = Native.poll(session, 100)
                if (events.length > 2) main.post { apply(events) }
            }
        }, "dictation-poll").apply { start() }
        capture = Thread(::record, "dictation-mic").apply { start() }
    }

    /** Close the utterance in progress; returns now on the session's clock (ms). */
    fun breakNow(): Long {
        breakAsked = true
        return pushed * 1000 / RATE
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
        var wasPaused = false
        try {
            while (capturing) {
                val n = rec.read(shorts, 0, shorts.size, AudioRecord.READ_BLOCKING)
                if (n <= 0) continue
                if (breakAsked) {
                    breakAsked = false
                    Native.breakParagraph(session)
                }
                val p = paused
                if (p != wasPaused) {
                    wasPaused = p
                    // Close the utterance the pause cut off. Here rather
                    // than on the main thread so it can't race a push.
                    if (p) Native.breakParagraph(session)
                }
                if (p) continue
                for (i in 0 until n) buf[i] = shorts[i] / 32768f
                gain.process(buf, n)
                Native.push(session, buf, n)
                pushed += n
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
        var changed = false
        for (i in 0 until events.length()) {
            val ev = events.getJSONObject(i)
            when (ev.getString("t")) {
                "paragraph" -> parseKPara(ev.getJSONObject("p")).let { paras[it.id] = it; changed = true }
                "removed" -> changed = paras.remove(ev.getString("id")) != null || changed
                "level" -> if (!paused) onLevel(ev.getDouble("rms").toFloat())
            }
        }
        // Level events arrive every 50 ms; only text changes redraw.
        if (changed) onChange(paras.values.toList())
    }

    /** Stop listening, let the passes finish, then hand back the final paragraphs on [main]. */
    fun finish(done: (List<KPara>) -> Unit) = end { json ->
        val list = JSONObject(json).optJSONArray("paragraphs") ?: JSONArray()
        val final = (0 until list.length()).map { parseKPara(list.getJSONObject(it)) }
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
            main.post { Scribe.releaseFromDictation() }
            then(json)
        }, "dictation-finish").start()
    }
}
