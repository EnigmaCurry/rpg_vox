import AVFoundation
import Foundation

/// Sample rate the engine runs at, as on Android.
let RATE = 16_000

/// Mic gain: automatic, or a fixed boost in dB (0 = as recorded).
enum Boost: String, CaseIterable, Identifiable {
    case auto, off, db6, db12, db18

    var id: String { rawValue }

    var label: String {
        switch self {
        case .auto: "Auto (raise quiet speech)"
        case .off: "Off"
        case .db6: "+6 dB"
        case .db12: "+12 dB"
        case .db18: "+18 dB"
        }
    }

    var db: Float? {
        switch self {
        case .auto: nil
        case .off: 0
        case .db6: 6
        case .db12: 12
        case .db18: 18
        }
    }
}

/// Which microphone a recording uses.
enum MicRoute: String, CaseIterable, Identifiable {
    /// A Bluetooth headset's mic when one is connected, else the device's.
    case auto
    case device

    var id: String { rawValue }

    var label: String {
        switch self {
        case .auto: "Bluetooth headset when connected"
        case .device: "Built-in mic always"
        }
    }
}

/// Software gain for the mic, applied before the engine (so its voice
/// detector and the level meter see the boosted signal), as on Android.
///
/// Auto follows the speech level, not the noise: the speech estimate
/// only moves on blocks well above the tracked noise floor, the gain aims
/// that at `target` and never lifts the noise floor past `noiseCeiling`
/// (the engine's voice detector opens at 0.008), falls quickly and rises
/// slowly. Both modes end in a soft limiter so peaks don't clip.
final class MicGain {
    /// Speech RMS to aim for, about -20 dBFS.
    private let target: Float = 0.1
    private let noiseCeiling: Float = 0.003
    private let maxGain: Float = 16 // +24 dB
    /// Per ~100 ms block: rise ~1.5 dB, fall ~6 dB.
    private let rise = powf(10, 1.5 / 20)
    private let fall = powf(10, -6 / 20)

    private let boost: Boost
    private(set) var gain: Float
    private var noise: Float = 0.002
    private var speech: Float = 0

    init(_ boost: Boost) {
        self.boost = boost
        gain = boost.db.map { powf(10, $0 / 20) } ?? 1
    }

    func process(_ buf: UnsafeMutablePointer<Float>, _ n: Int) {
        if boost == .auto && n > 0 { adapt(buf, n) }
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

/// The audio session for listening: the mic in iOS's normal mode (its
/// own gain on; measurement mode leaves speech too quiet), a Bluetooth
/// headset's mic when `route` allows.
func activateRecordingSession(_ route: MicRoute) throws {
    let s = AVAudioSession.sharedInstance()
    try s.setCategory(.playAndRecord, mode: .default,
                      options: route == .auto ? [.allowBluetoothHFP, .defaultToSpeaker] : [.defaultToSpeaker])
    try s.setActive(true)
    if route == .device, let mic = s.availableInputs?.first(where: { $0.portType == .builtInMic }) {
        try? s.setPreferredInput(mic)
    } else {
        try? s.setPreferredInput(nil)
    }
}

/// Name of the input in use, as the status line shows it.
func inputName() -> String? {
    guard let input = AVAudioSession.sharedInstance().currentRoute.inputs.first else { return nil }
    return input.portType == .builtInMic ? "Built-in mic" : input.portName
}

/// The mic, as 16 kHz mono float blocks through `MicGain`, handed to
/// `onSamples` on the audio thread. Rebuilds itself when the hardware
/// changes under it (a headset coming or going).
final class MicCapture: @unchecked Sendable {
    private let engine = AVAudioEngine()
    private let gain: MicGain
    private let route: MicRoute
    private let onSamples: (UnsafeMutablePointer<Float>, Int) -> Void
    private let onName: (String?) -> Void
    private var observers: [NSObjectProtocol] = []
    private var running = false
    /// Held while a block is handed over; `stop` takes it to close the gate,
    /// so no block reaches the session once it returns.
    private let gate = NSLock()
    private var open = true

    init(boost: Boost, route: MicRoute,
         onName: @escaping (String?) -> Void = { _ in },
         onSamples: @escaping (UnsafeMutablePointer<Float>, Int) -> Void) {
        gain = MicGain(boost)
        self.route = route
        self.onName = onName
        self.onSamples = onSamples
    }

    func start() throws {
        try activateRecordingSession(route)
        try run()
        running = true
        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main) { [weak self] _ in
            guard let self, self.running else { return }
            print("mic: configuration changed; restarting")
            self.engine.inputNode.removeTap(onBus: 0)
            try? self.run()
        })
        observers.append(nc.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main) { [weak self] _ in
            self?.onName(inputName())
        })
        observers.append(nc.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] note in
            guard let self, self.running,
                  let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  AVAudioSession.InterruptionType(rawValue: raw) == .ended else { return }
            // A call or Siri took the mic; take it back.
            try? AVAudioSession.sharedInstance().setActive(true)
            if !self.engine.isRunning { try? self.engine.start() }
        })
    }

    private func run() throws {
        let input = engine.inputNode
        let from = input.outputFormat(forBus: 0)
        guard from.sampleRate > 0,
              let to = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(RATE), channels: 1, interleaved: false),
              let converter = AVAudioConverter(from: from, to: to)
        else { throw NativeError(message: "no microphone input") }
        let ratio = to.sampleRate / from.sampleRate
        let gain = gain
        input.installTap(onBus: 0, bufferSize: 4096, format: from) { buf, _ in
            let cap = AVAudioFrameCount(Double(buf.frameLength) * ratio) + 16
            guard let out = AVAudioPCMBuffer(pcmFormat: to, frameCapacity: cap) else { return }
            var fed = false
            var error: NSError?
            converter.convert(to: out, error: &error) { _, status in
                if fed {
                    status.pointee = .noDataNow
                    return nil
                }
                fed = true
                status.pointee = .haveData
                return buf
            }
            guard error == nil, let ch = out.floatChannelData, out.frameLength > 0 else { return }
            let n = Int(out.frameLength)
            gain.process(ch[0], n)
            self.gate.lock()
            if self.open { self.onSamples(ch[0], n) }
            self.gate.unlock()
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw error
        }
        print("mic: \(inputName() ?? "?") \(from)")
        onName(inputName())
    }

    /// Stops the mic; no `onSamples` call starts after this returns.
    func stop() {
        gate.lock()
        open = false
        gate.unlock()
        running = false
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        onName(nil)
    }
}
