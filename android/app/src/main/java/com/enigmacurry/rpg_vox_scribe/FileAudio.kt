package com.enigmacurry.rpg_vox_scribe

import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import java.io.File
import java.io.IOException
import java.nio.ByteOrder

/**
 * Decodes the first audio track of [file] (anything Android's codecs
 * play: m4a, mp3, opus, ogg, flac, wav, video files…) to mono at
 * [rate] Hz, handing it to [sink] in blocks. [durationMs] is called
 * once up front (0 when unknown). Stops early when [cancelled] says so.
 */
fun decodeAudio(
    file: File,
    rate: Int,
    durationMs: (Long) -> Unit,
    cancelled: () -> Boolean,
    sink: (FloatArray, Int) -> Unit,
) {
    val ex = MediaExtractor()
    try {
        ex.setDataSource(file.path)
        val track = (0 until ex.trackCount).firstOrNull {
            ex.getTrackFormat(it).getString(MediaFormat.KEY_MIME)?.startsWith("audio/") == true
        } ?: throw IOException("no audio track")
        ex.selectTrack(track)
        val format = ex.getTrackFormat(track)
        durationMs(if (format.containsKey(MediaFormat.KEY_DURATION)) format.getLong(MediaFormat.KEY_DURATION) / 1000 else 0)
        val codec = MediaCodec.createDecoderByType(format.getString(MediaFormat.KEY_MIME)!!)
        codec.configure(format, null, null, 0)
        codec.start()
        try {
            pump(ex, codec, rate, cancelled, sink)
        } finally {
            codec.stop()
            codec.release()
        }
    } finally {
        ex.release()
    }
}

private fun pump(
    ex: MediaExtractor,
    codec: MediaCodec,
    rate: Int,
    cancelled: () -> Boolean,
    sink: (FloatArray, Int) -> Unit,
) {
    val info = MediaCodec.BufferInfo()
    var inputDone = false
    var resampler: Resampler? = null
    var channels = 1
    var float = false
    while (!cancelled()) {
        if (!inputDone) {
            val i = codec.dequeueInputBuffer(10_000)
            if (i >= 0) {
                val n = ex.readSampleData(codec.getInputBuffer(i)!!, 0)
                if (n < 0) {
                    codec.queueInputBuffer(i, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                    inputDone = true
                } else {
                    codec.queueInputBuffer(i, 0, n, ex.sampleTime, 0)
                    ex.advance()
                }
            }
        }
        val o = codec.dequeueOutputBuffer(info, 10_000)
        when {
            o == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                val f = codec.outputFormat
                channels = f.getInteger(MediaFormat.KEY_CHANNEL_COUNT)
                float = f.containsKey(MediaFormat.KEY_PCM_ENCODING) &&
                    f.getInteger(MediaFormat.KEY_PCM_ENCODING) == AudioFormat.ENCODING_PCM_FLOAT
                resampler = Resampler(f.getInteger(MediaFormat.KEY_SAMPLE_RATE), rate)
            }
            o >= 0 -> {
                val buf = codec.getOutputBuffer(o)!!.order(ByteOrder.nativeOrder())
                buf.position(info.offset)
                buf.limit(info.offset + info.size)
                val frames: Int
                val mono: FloatArray
                if (float) {
                    val fb = buf.asFloatBuffer()
                    frames = fb.remaining() / channels
                    mono = FloatArray(frames) { i ->
                        var sum = 0f
                        for (c in 0 until channels) sum += fb.get(i * channels + c)
                        sum / channels
                    }
                } else {
                    val sb = buf.asShortBuffer()
                    frames = sb.remaining() / channels
                    mono = FloatArray(frames) { i ->
                        var sum = 0f
                        for (c in 0 until channels) sum += sb.get(i * channels + c)
                        sum / channels / 32768f
                    }
                }
                codec.releaseOutputBuffer(o, false)
                val r = resampler ?: Resampler(codec.outputFormat.getInteger(MediaFormat.KEY_SAMPLE_RATE), rate)
                    .also { resampler = it }
                val out = r.process(mono, frames)
                if (out.isNotEmpty()) sink(out, out.size)
                if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) return
            }
        }
    }
}

/**
 * Box-filter downsampler (or linear upsampler): each output sample
 * averages the input samples it covers, which keeps aliasing low enough
 * for speech recognition without a real filter.
 */
private class Resampler(private val from: Int, private val to: Int) {
    private val step = from.toDouble() / to
    /** Input position (in input samples, relative to the next block) of the next output sample's start. */
    private var pos = 0.0
    private var carry = FloatArray(0)

    fun process(input: FloatArray, n: Int): FloatArray {
        if (from == to) return input.copyOf(n)
        val data = carry + input.copyOf(n)
        var count = 0
        while (pos + (count + 1) * step <= data.size) count++
        val out = FloatArray(count)
        for (k in 0 until count) {
            val p = pos + k * step
            val a = p.toInt()
            out[k] = if (step >= 1) {
                val b = (p + step).toInt().coerceAtMost(data.size)
                var sum = 0f
                for (i in a until b) sum += data[i]
                if (b > a) sum / (b - a) else data[a]
            } else {
                val t = (p - a).toFloat()
                val next = if (a + 1 < data.size) data[a + 1] else data[a]
                data[a] * (1 - t) + next * t
            }
        }
        pos += count * step
        val keep = pos.toInt()
        carry = data.copyOfRange(keep, data.size)
        pos -= keep
        return out
    }
}
