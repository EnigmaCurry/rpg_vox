import Foundation

/// Automatic software gain for the mic, ported from the Android app's
/// MicGain (Auto): applied before the engine, so its voice detector and
/// the level meter see the boosted signal.
///
/// It follows the speech level, not the noise: the speech estimate only
/// moves on blocks well above the tracked noise floor, the gain aims that
/// at `target` and never lifts the noise floor past `noiseCeiling` (the
/// engine's voice detector opens at 0.008), falls quickly and rises
/// slowly. A soft limiter keeps peaks from clipping.
final class MicGain: @unchecked Sendable {
    /// Speech RMS to aim for, about -20 dBFS.
    private let target: Float = 0.1
    private let noiseCeiling: Float = 0.003
    private let maxGain: Float = 16 // +24 dB
    /// Per ~100 ms block: rise ~1.5 dB, fall ~6 dB.
    private let rise = powf(10, 1.5 / 20)
    private let fall = powf(10, -6 / 20)

    private(set) var gain: Float = 1
    private var noise: Float = 0.002
    private var speech: Float = 0

    func process(_ buf: UnsafeMutablePointer<Float>, _ n: Int) {
        guard n > 0 else { return }
        adapt(buf, n)
        if gain == 1 { return }
        for i in 0..<n { buf[i] = limit(buf[i] * gain) }
    }

    private func adapt(_ buf: UnsafeMutablePointer<Float>, _ n: Int) {
        var sum: Float = 0
        for i in 0..<n { sum += buf[i] * buf[i] }
        let rms = (sum / Float(n)).squareRoot()
        // Noise floor: drops at once, creeps up slowly through speech.
        noise = rms < noise ? max(rms, 1e-5) : noise * 1.02
        if rms > noise * 3 && rms > 0.0005 {
            // Speech: follow its loudness, faster up than down.
            speech += (rms - speech) * (rms > speech ? 0.5 : 0.05)
        }
        guard speech > 0 else { return }
        let want = max(min(target / speech, noiseCeiling / noise, maxGain), 1)
        gain = want < gain ? max(want, gain * fall) : min(want, gain * rise)
    }

    /// Linear below 0.8, then a tanh knee that never reaches 1.
    private func limit(_ x: Float) -> Float {
        let a = abs(x)
        if a <= 0.8 { return x }
        let over = tanhf((a - 0.8) / 0.2) * 0.2
        return x < 0 ? -(0.8 + over) : 0.8 + over
    }
}
