package com.enigmacurry.rpg_vox_scribe

import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.media.AudioRecord
import android.os.Build
import android.util.Log
import kotlin.math.abs
import kotlin.math.log10
import kotlin.math.pow
import kotlin.math.sqrt
import kotlin.math.tanh

/** Mic gain: automatic, or a fixed boost in dB (0 = as recorded). */
enum class Boost(val label: String, val db: Float?) {
    AUTO("Auto", null),
    OFF("Off", 0f),
    DB6("+6 dB", 6f),
    DB12("+12 dB", 12f),
    DB18("+18 dB", 18f),
}

/** Which microphone a recording uses. */
enum class MicRoute(val label: String) {
    /** A Bluetooth headset's mic when one is connected, else the phone's. */
    AUTO("Bluetooth headset when connected"),
    PHONE("Phone mic always"),
}

/**
 * Software gain for the mic, applied before the engine (so its voice
 * detector and the level meter see the boosted signal).
 *
 * Auto follows the speech level, not the noise: the speech estimate
 * only moves on blocks well above the tracked noise floor, the gain aims
 * that at [TARGET] and never lifts the noise floor past [NOISE_CEILING]
 * (the engine's voice detector opens at 0.008), falls quickly and rises
 * slowly. Both modes end in a soft limiter so peaks don't clip.
 */
class MicGain(private val boost: Boost) {
    private companion object {
        /** Speech RMS to aim for, about -20 dBFS. */
        const val TARGET = 0.1f
        const val NOISE_CEILING = 0.003f
        const val MAX_GAIN = 16f // +24 dB
        /** Per 100 ms block: rise ~1.5 dB, fall ~6 dB. */
        val RISE = 10f.pow(1.5f / 20)
        val FALL = 10f.pow(-6f / 20)
    }

    private var gain = boost.db?.let { 10f.pow(it / 20) } ?: 1f
    private var noise = 0.002f
    private var speech = 0f

    fun process(buf: FloatArray, n: Int) {
        if (boost == Boost.AUTO && n > 0) adapt(buf, n)
        if (gain == 1f) return
        for (i in 0 until n) buf[i] = limit(buf[i] * gain)
    }

    private fun adapt(buf: FloatArray, n: Int) {
        var sum = 0f
        for (i in 0 until n) sum += buf[i] * buf[i]
        val rms = sqrt(sum / n)
        // Noise floor: drops at once, creeps up slowly through speech.
        noise = if (rms < noise) rms.coerceAtLeast(1e-5f) else noise * 1.02f
        if (rms > noise * 3 && rms > 0.0005f) {
            // Speech: follow its loudness, faster up than down.
            speech = if (rms > speech) speech + (rms - speech) * 0.5f else speech + (rms - speech) * 0.05f
        }
        if (speech <= 0f) return
        val want = minOf(TARGET / speech, NOISE_CEILING / noise, MAX_GAIN).coerceAtLeast(1f)
        gain = when {
            want < gain -> maxOf(want, gain * FALL)
            else -> minOf(want, gain * RISE)
        }
    }

    /** Linear below 0.8, then a tanh knee that never reaches 1. */
    private fun limit(x: Float): Float {
        val a = abs(x)
        if (a <= 0.8f) return x
        val over = tanh(((a - 0.8f) / 0.2f).toDouble()).toFloat() * 0.2f
        return if (x < 0) -(0.8f + over) else 0.8f + over
    }

    fun gainDb(): Float = 20 * log10(gain)
}

/**
 * Routes recording to a Bluetooth headset mic: Android only uses one
 * when an app asks, by making it the communication device (which brings
 * up the headset's call link). [release] puts routing back.
 */
class MicRouting(private val am: AudioManager) {
    private var routed = false

    /** The headset input to prefer, or null to use the phone's mic. */
    fun headset(): AudioDeviceInfo? {
        if (Build.VERSION.SDK_INT < 31) return null
        val out = am.availableCommunicationDevices.firstOrNull {
            it.type == AudioDeviceInfo.TYPE_BLE_HEADSET || it.type == AudioDeviceInfo.TYPE_BLUETOOTH_SCO
        } ?: return null
        routed = am.setCommunicationDevice(out)
        if (!routed) {
            Log.w("Scribe", "could not route to ${out.productName}")
            return null
        }
        val inType = if (out.type == AudioDeviceInfo.TYPE_BLE_HEADSET) {
            AudioDeviceInfo.TYPE_BLE_HEADSET
        } else {
            AudioDeviceInfo.TYPE_BLUETOOTH_SCO
        }
        return am.getDevices(AudioManager.GET_DEVICES_INPUTS).firstOrNull { it.type == inType }
    }

    fun release() {
        if (routed && Build.VERSION.SDK_INT >= 31) am.clearCommunicationDevice()
        routed = false
    }
}

/** A short name for the input a recorder is using. */
fun AudioRecord.inputName(): String {
    val d = routedDevice ?: return "Phone mic"
    return when (d.type) {
        AudioDeviceInfo.TYPE_BUILTIN_MIC -> "Phone mic"
        else -> d.productName?.toString()?.takeIf { it.isNotBlank() } ?: "External mic"
    }
}
