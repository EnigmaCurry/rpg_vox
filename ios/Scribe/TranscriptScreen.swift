import AVFoundation
import SwiftUI

/// Asks for the mic, then runs `go`; tells the user how to allow it otherwise.
@MainActor
func withMic(_ go: @escaping () -> Void, denied: @escaping () -> Void) {
    switch AVAudioApplication.shared.recordPermission {
    case .granted:
        go()
    case .denied:
        denied()
    default:
        AVAudioApplication.requestRecordPermission { ok in
            Task { @MainActor in ok ? go() : denied() }
        }
    }
}

let MIC_DENIED = "Microphone access is off: Settings › Privacy & Security › Microphone › Scribe."

/// The open transcription: its transcript, and the dock for recording,
/// a file being transcribed, or playback.
struct TranscriptScreen: View {
    @Environment(Scribe.self) private var scribe
    @Environment(Playback.self) private var playback
    @State private var confirmClear = false
    @State private var now: Int64 = 0

    var body: some View {
        VStack(spacing: 0) {
            Transcript()
            dock
        }
        .task(id: scribe.recording) {
            // Ticks the elapsed time once a second while a session is open.
            while scribe.recording && !Task.isCancelled {
                now = scribe.elapsedMs()
                try? await Task.sleep(nanoseconds: UInt64(1_000 - now % 1_000) * 1_000_000)
            }
        }
        .task(id: playback.playing) {
            while playback.playing != nil && !Task.isCancelled {
                now = playback.positionMs()
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
        }
        .task(id: scribe.startWhenOpen) {
            // Made with New, with "Start recording on New" on.
            guard scribe.startWhenOpen == "transcription" else { return }
            scribe.startWhenOpen = nil
            if !scribe.busy() { toggle() }
        }
        .confirmationDialog("Clear this thread?", isPresented: $confirmClear, titleVisibility: .visible) {
            Button("Clear", role: .destructive) { scribe.clear() }
        } message: {
            let n = scribe.recordings.count
            Text("Its text" + (n > 0 ? " and \(n) recording(s)" : "") + " will be deleted. The thread itself stays. This can't be undone.")
        }
    }

    private func toggle() {
        if scribe.recording || scribe.file != nil {
            scribe.stop()
        } else {
            withMic({ scribe.start() }, denied: { scribe.notice = MIC_DENIED })
        }
    }

    private var copyButton: some View {
        DockButton(icon: "doc.on.doc", label: "Copy", enabled: !scribe.paragraphs.isEmpty) {
            UIPasteboard.general.string = scribe.text()
        }
    }

    @ViewBuilder private var dock: some View {
        if playback.playing != nil {
            // Playback: its own controls; Stop leaves it.
            Dock(
                status: (playback.paused ? "Paused · " : "") + clock(now) + (playback.lengthMs > 0 ? " / \(clock(playback.lengthMs))" : ""),
                live: false,
                l2: { DockButton(icon: "stop.fill", label: "Stop playing") { playback.stop() } },
                l1: { DockButton(icon: "gobackward.10", label: "Back 10 seconds") { playback.skip(-10_000) } },
                center: {
                    BigButton(label: playback.paused ? "Resume" : "Pause",
                              icon: playback.paused ? "play.fill" : "pause.fill",
                              ring: playback.lengthMs > 0 ? Double(now) / Double(playback.lengthMs) : 0) {
                        playback.paused ? playback.resume() : playback.pause()
                    }
                },
                r1: { DockButton(icon: "goforward.10", label: "Ahead 10 seconds") { playback.skip(10_000) } },
                r2: { copyButton }
            )
        } else if scribe.busy() {
            // Recording, or transcribing a file.
            let job = scribe.file
            Dock(
                status: busyStatus(job),
                live: scribe.recording && !scribe.paused && !scribe.finishing,
                l2: { EmptyView() },
                l1: { EmptyView() },
                center: {
                    BigButton(label: job != nil ? "Stop transcribing" : "Stop recording",
                              enabled: !scribe.finishing, recording: true,
                              ring: job.map { max($0.progress, 0) } ?? (scribe.recording && !scribe.paused ? Double(scribe.level) * 8 : 0),
                              action: toggle)
                },
                r1: {
                    if scribe.recording && !scribe.finishing {
                        DockButton(icon: scribe.paused ? "play.fill" : "pause.fill", label: scribe.paused ? "Resume" : "Pause") {
                            scribe.paused ? scribe.resume() : scribe.pause()
                        }
                    }
                },
                r2: { copyButton }
            )
        } else {
            Dock(
                status: scribe.paragraphs.contains(where: \.playable) ? "Tap ● to record · tap a word to play" : "Tap ● to record",
                live: false,
                l2: { EmptyView() },
                l1: {
                    // Also clears audio that has no text with it.
                    DockButton(icon: "trash", label: "Clear", enabled: !scribe.paragraphs.isEmpty || !scribe.threadAudio.isEmpty) {
                        confirmClear = true
                    }
                },
                center: { BigButton(label: "Start recording", action: toggle) },
                r1: { EmptyView() },
                r2: { copyButton }
            )
        }
    }

    private func busyStatus(_ job: FileJob?) -> String {
        if let job {
            return "Transcribing \(job.name)" + (job.detail().map { " · \($0)" } ?? (job.progress >= 0 ? " · \(Int(job.progress * 100))%" : "…"))
        }
        if scribe.finishing { return "Finishing…" }
        if scribe.paused { return "Paused \(clock(now))" }
        return "Listening \(clock(now))" + (scribe.micName.map { " · \($0)" } ?? "")
    }
}

/// When `recording` began (epoch ms), from its name (yyyyMMdd-HHmmss-…).
private func recordingStart(_ recording: String) -> Int64 {
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.dateFormat = "yyyyMMdd-HHmmss"
    guard let d = f.date(from: String(recording.prefix(15))) else { return 0 }
    return Int64(d.timeIntervalSince1970 * 1000)
}

/// The last word of `playing` to have started by `pos`.
@MainActor
private func locate(_ paras: [Para], _ playing: Playing, _ pos: Int64) -> Karaoke? {
    var best: Karaoke?
    for p in paras {
        guard let a = p.audio, let off = playing.offsets[a], !p.words.isEmpty else { continue }
        let shown = p.shown.count
        for i in 0..<shown {
            let t = off + p.wordAt(i, shown).startMs
            if t <= pos && (best == nil || t >= best!.ms) { best = Karaoke(para: p.id, word: i, ms: t) }
        }
    }
    return best
}

/// The transcript: follows the newest text until the reader scrolls up
/// (Jump to latest resumes it); faint live text; finished paragraphs you
/// can tap to play from, with their recording's ▶ above the first.
private struct Transcript: View {
    @Environment(Scribe.self) private var scribe
    @Environment(Playback.self) private var playback
    @State private var follow = true
    @State private var at: Karaoke?
    @State private var editing: Para?

    var body: some View {
        let paras = scribe.paragraphs
        if paras.isEmpty {
            empty
        } else {
            list(paras)
        }
    }

    @ViewBuilder private var empty: some View {
        // Kept audio with nothing transcribed still plays, from the start.
        let rec = scribe.keepNonSpeech ? scribe.threadAudio.values.max() : nil
        VStack(spacing: 16) {
            if let rec, !scribe.busy() {
                PlayFromStart(recording: rec, big: true)
                Text("No words detected in log").foregroundStyle(.secondary)
            } else {
                Text("Your words appear here. Live text is shown faint, then replaced by the final transcription a moment later.")
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func list(_ paras: [Para]) -> some View {
        // Recordings in which nothing was transcribed, each shown just before
        // the first paragraph said after it started (or at the end); only
        // when non-speech audio is kept, otherwise there's nothing to play.
        let heard = Set(paras.compactMap { p in p.audio.flatMap { scribe.threadAudio[$0] } })
        var before: [String: [String]] = [:]
        var tail: [String] = []
        for rec in scribe.recordings where scribe.keepNonSpeech && !heard.contains(rec) {
            let start = recordingStart(rec)
            if let p = paras.first(where: { $0.atMs > start }) {
                before[p.id, default: []].append(rec)
            } else {
                tail.append(rec)
            }
        }
        // The first paragraph of each recording, which gets a play-from-start button.
        var firsts: [String: String] = [:]
        var seen = Set<String>()
        for p in paras {
            guard let a = p.audio, let rec = scribe.threadAudio[a], !seen.contains(rec) else { continue }
            seen.insert(rec)
            firsts[p.id] = rec
        }
        let busy = scribe.busy()
        let held = playback.paused
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(Array(paras.enumerated()), id: \.element.id) { i, p in
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(before[p.id] ?? [], id: \.self) { NoWords(recording: $0) }
                            // Label each run of paragraphs from one speaker.
                            if let s = p.source, i == 0 || paras[i - 1].source != s {
                                Text(s).font(.subheadline.weight(.semibold)).foregroundStyle(.tint)
                            }
                            if let rec = firsts[p.id], !busy { PlayFromStart(recording: rec, big: false) }
                            if p.partial {
                                partialText(p)
                            } else {
                                ParagraphText(para: p, playing: playback.playing, at: playback.playing != nil ? at : nil,
                                              canPlay: !busy && p.playable, busy: busy, fontStep: scribe.fontStep,
                                              onEdit: { editing = $0 })
                            }
                        }
                        .id(p.id)
                    }
                    ForEach(tail, id: \.self) { NoWords(recording: $0) }
                    Color.clear.frame(height: 1).id("end")
                        .onAppear { follow = true }
                }
                .padding(16)
                .frame(maxWidth: 820, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .simultaneousGesture(DragGesture(minimumDistance: 8).onChanged { _ in
                if playback.playing == nil { follow = false }
            })
            .onChange(of: paras.last?.text) {
                if follow { proxy.scrollTo("end", anchor: .bottom) }
            }
            .onChange(of: paras.count) {
                if follow { proxy.scrollTo("end", anchor: .bottom) }
            }
            .onAppear { proxy.scrollTo("end", anchor: .bottom) }
            .task(id: playback.playing) {
                // Playback: the word being heard, followed down the transcript.
                at = nil
                guard let playing = playback.playing else { return }
                while !Task.isCancelled {
                    let k = locate(scribe.paragraphs, playing, playback.positionMs())
                    if k != at {
                        // Paused, the reader scrolls freely; resuming brings the word back.
                        if k?.para != at?.para, let id = k?.para, !playback.paused {
                            withAnimation { proxy.scrollTo(id, anchor: .center) }
                        }
                        at = k
                    }
                    try? await Task.sleep(nanoseconds: 33_000_000)
                }
            }
            .overlay(alignment: .bottom) {
                if !follow && playback.playing == nil {
                    Button {
                        follow = true
                        withAnimation { proxy.scrollTo("end", anchor: .bottom) }
                    } label: {
                        Label("Jump to latest", systemImage: "arrow.down")
                    }
                    .buttonStyle(.borderedProminent)
                    .padding(.bottom, 12)
                }
            }
            .onChange(of: held) {
                if !held, let id = at?.para { withAnimation { proxy.scrollTo(id, anchor: .center) } }
            }
        }
        .sheet(item: $editing) { p in
            EditParagraph(para: p)
        }
    }

    /// Live text: finished clips as they are, pass-1 partials faint (and
    /// lowercased: Zipformer's are ALL CAPS).
    private func partialText(_ p: Para) -> some View {
        let size = UIFont.preferredFont(forTextStyle: .body).pointSize * FONT_SCALES[scribe.fontStep]
        let clips = p.clips.filter { !$0.text.trimmingCharacters(in: .whitespaces).isEmpty }
        var out = AttributedString()
        for (i, c) in clips.enumerated() {
            if i > 0 { out += AttributedString(" ") }
            var s = AttributedString(c.partial ? c.text.lowercased() : c.text)
            if c.partial {
                s.foregroundColor = .secondary
                s.font = .system(size: size).italic()
            }
            out += s
        }
        return Text(out).font(.system(size: size)).frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Play `recording` from 0:00: small above its first paragraph, or `big`
/// when it has no words. Its menu deletes that one recording.
private struct PlayFromStart: View {
    @Environment(Scribe.self) private var scribe
    let recording: String
    let big: Bool
    @State private var confirm = false

    var body: some View {
        Button {
            scribe.playFromStart(recording)
        } label: {
            Image(systemName: "play.fill")
                .font(.system(size: big ? 36 : 14))
                .foregroundStyle(.white)
                .frame(width: big ? 96 : 34, height: big ? 96 : 34)
                .background(Circle().fill(Color(red: 0.18, green: 0.49, blue: 0.2)))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Play the recording from the start")
        .contextMenu {
            Button("Delete clip", systemImage: "trash", role: .destructive) { confirm = true }
        }
        .confirmationDialog("Delete this clip?", isPresented: $confirm, titleVisibility: .visible) {
            Button("Delete", role: .destructive) { scribe.deleteRecording(recording) }
        } message: {
            Text("Its audio and the text transcribed from it will be deleted. The rest of the thread stays. This can't be undone.")
        }
    }
}

/// A recording in the log with no words: play the thread from its start.
private struct NoWords: View {
    @Environment(Scribe.self) private var scribe
    let recording: String

    var body: some View {
        HStack(spacing: 8) {
            if !scribe.busy() { PlayFromStart(recording: recording, big: false) }
            Text("No words detected").italic().foregroundStyle(.secondary)
        }
        .padding(.bottom, 8)
    }
}

/// Retype a paragraph; word times carry over where words still match.
private struct EditParagraph: View {
    @Environment(\.dismiss) private var dismiss
    let para: Para
    @State private var text = ""

    var body: some View {
        NavigationStack {
            TextEditor(text: $text)
                .font(.body)
                .padding()
                .navigationTitle("Edit")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            Scribe.shared.edit(para, text)
                            dismiss()
                        }
                    }
                }
        }
        .onAppear { text = para.text }
        .presentationDetents([.medium, .large])
    }
}
