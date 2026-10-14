import SwiftUI
import UniformTypeIdentifiers

/// A transcription (thread) and a dictation, as the list marks them.
private let MIC = "🎙️"
private let PEN = "✍️"

/// Pages of settings, from the ⋯ menu.
enum SettingsPage: String, Identifiable, CaseIterable {
    case microphone = "Microphone"
    case recording = "Recording"
    case display = "Display"
    case about = "About"

    var id: String { rawValue }
}

/// Something to rename or delete, from the list or the menu.
enum Target: Identifiable, Equatable {
    case thread(ThreadInfo)
    case dictation(DictationInfo)

    var id: String {
        switch self {
        case .thread(let t): "t\(t.id)"
        case .dictation(let d): "d\(d.id)"
        }
    }

    var title: String {
        switch self {
        case .thread(let t): t.title
        case .dictation(let d): d.title
        }
    }
}

/// The app: the list of dictations and transcriptions beside (or, on a
/// phone, before) the open one.
struct ContentView: View {
    @Environment(Scribe.self) private var scribe
    @Environment(Dictations.self) private var dictations
    @State private var columns = NavigationSplitViewVisibility.automatic
    @State private var settings: SettingsPage?
    @State private var renaming: Target?
    @State private var newTitle = ""
    @State private var deleting: Target?
    @State private var choosingNew = false
    @State private var importing = false

    var body: some View {
        NavigationSplitView(columnVisibility: $columns) {
            ThreadList(choosingNew: $choosingNew, renaming: rename, deleting: { deleting = $0 })
                .navigationTitle("Scribe")
        } detail: {
            NavigationStack {
                detail
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar { toolbar }
            }
        }
        .confirmationDialog("New", isPresented: $choosingNew, titleVisibility: .visible) {
            Button("\(PEN) Dictation: write by voice, and keep editing anywhere") {
                dictations.create()
                if scribe.autoStart { scribe.startWhenOpen = "dictation" }
                columns = .detailOnly
            }
            Button("\(MIC) Transcription: record and transcribe in order") {
                dictations.close()
                scribe.newThread()
                if scribe.autoStart { scribe.startWhenOpen = "transcription" }
                columns = .detailOnly
            }
        }
        .alert(renamingTitle, isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Title", text: $newTitle)
            Button("Rename") {
                switch renaming {
                case .thread(let t): scribe.rename(t.id, newTitle)
                case .dictation(let d): dictations.rename(d.id, newTitle)
                case nil: break
                }
                renaming = nil
            }
            .disabled(newTitle.trimmingCharacters(in: .whitespaces).isEmpty)
            Button("Cancel", role: .cancel) { renaming = nil }
        }
        .alert(deletingTitle, isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button("Delete", role: .destructive) {
                switch deleting {
                case .thread(let t): scribe.delete(t.id)
                case .dictation(let d): dictations.delete(d.id)
                case nil: break
                }
                deleting = nil
            }
            Button("Cancel", role: .cancel) { deleting = nil }
        } message: {
            switch deleting {
            case .thread(let t): Text("\"\(t.title)\" and its \(t.count) paragraph(s) will be deleted. This can't be undone.")
            case .dictation(let d): Text("\"\(d.title)\" and its text will be deleted. This can't be undone.")
            case nil: Text("")
            }
        }
        .alert("Scribe", isPresented: Binding(get: { notice != nil }, set: { if !$0 { clearNotice() } })) {
            Button("OK", role: .cancel) { clearNotice() }
        } message: {
            Text(notice ?? "")
        }
        .sheet(item: $settings) { SettingsView(page: $0) }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.audio, .movie, .audiovisualContent]) { result in
            if case .success(let url) = result {
                dictations.close()
                scribe.notice = scribe.transcribe(url)
            }
        }
    }

    private var notice: String? { scribe.notice ?? dictations.notice }

    private func clearNotice() {
        scribe.notice = nil
        dictations.notice = nil
    }

    private var renamingTitle: String {
        if case .dictation = renaming { return "Rename dictation" }
        return "Rename transcription"
    }

    private var deletingTitle: String {
        if case .dictation = deleting { return "Delete dictation?" }
        return "Delete transcription?"
    }

    private func rename(_ t: Target) {
        newTitle = t.title
        renaming = t
    }

    @ViewBuilder private var detail: some View {
        switch scribe.phase {
        case .needModels(let error, let resume): Setup(error: error, resume: resume)
        case .installing(let step, let overall): Centered(text: "\(step.text)\n\nThis carries on if you switch apps.", progress: overall)
        case .loading: Centered(text: "Loading speech models…", progress: nil)
        case .unloaded:
            VStack(spacing: 16) {
                Text("Speech models are unloaded to free memory.")
                Button("Load models") { scribe.load() }.buttonStyle(.borderedProminent)
            }
        case .failed(let e): Centered(text: "Could not load the models:\n\(e)", progress: nil)
        case .ready:
            if dictations.open != nil { DictationScreen() } else { TranscriptScreen() }
        }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        // The open thread; tap to rename it.
        ToolbarItem(placement: .principal) {
            if scribe.phase == .ready {
                Button {
                    if let d = dictations.open { rename(.dictation(d)) } else { rename(.thread(scribe.thread)) }
                } label: {
                    Text(dictations.open?.title ?? scribe.thread.title).font(.headline).lineLimit(1)
                }
                .buttonStyle(.plain)
            } else {
                Text("Scribe").font(.headline)
            }
        }
        ToolbarItem(placement: .primaryAction) {
            Menu {
                if scribe.phase == .ready {
                    let text = dictations.open != nil ? dictations.text() : scribe.text()
                    ShareLink(item: text) { Label("Share", systemImage: "square.and.arrow.up") }
                        .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    Button("Rename", systemImage: "pencil") {
                        if let d = dictations.open { rename(.dictation(d)) } else { rename(.thread(scribe.thread)) }
                    }
                    Button("Delete", systemImage: "trash", role: .destructive) {
                        if let d = dictations.open {
                            deleting = .dictation(d)
                        } else {
                            deleting = .thread(scribe.threadList.first { $0.id == scribe.thread.id } ?? scribe.thread)
                        }
                    }
                    .disabled(dictations.open == nil && scribe.busy())
                    Button("Transcribe a file…", systemImage: "waveform") { importing = true }
                        .disabled(scribe.busy() || dictations.listening)
                    Divider()
                }
                ForEach(SettingsPage.allCases) { page in
                    Button(page.rawValue) { settings = page }
                }
            } label: {
                Label("More", systemImage: "ellipsis.circle")
            }
        }
    }
}

/// The list: New, which asks whether for a transcription or a dictation,
/// then both kinds together, the most recently changed first, each with
/// its time and last words and marked by its icon. Tap opens one; its
/// menu offers Rename and Delete. While recording only the open
/// transcription is reachable.
private struct ThreadList: View {
    @Environment(Scribe.self) private var scribe
    @Environment(Dictations.self) private var dictations
    @Binding var choosingNew: Bool
    let renaming: (Target) -> Void
    let deleting: (Target) -> Void

    var body: some View {
        let locked = scribe.busy()
        // Both kinds' times are local "YYYY-MM-DD HH:MM", so they sort as text.
        let entries = (scribe.threadList.map { ($0.last, Target.thread($0)) } + dictations.list.map { ($0.updated, Target.dictation($0)) })
            .sorted { $0.0 > $1.0 }
            .map(\.1)
        List {
            if locked {
                Text("Stop recording to switch").font(.footnote).foregroundStyle(.red)
            }
            Button {
                choosingNew = true
            } label: {
                Label("New", systemImage: "plus")
            }
            .disabled(locked || scribe.phase != .ready)
            Section {
                ForEach(entries) { e in
                    row(e, locked: locked)
                }
            }
        }
        .listStyle(.sidebar)
    }

    @ViewBuilder private func row(_ e: Target, locked: Bool) -> some View {
        let (icon, title, subtitle, selected, enabled): (String, String, String, Bool, Bool) = {
            switch e {
            case .dictation(let d):
                let words = String(d.text.prefix(80)).replacingOccurrences(of: "\n", with: " ")
                return (PEN, d.title, d.updated + (words.trimmingCharacters(in: .whitespaces).isEmpty ? "" : " · \(words)"),
                        d.id == dictations.open?.id, !locked)
            case .thread(let t):
                let open = t.id == scribe.thread.id
                return (MIC, t.title, t.last + (t.preview.isEmpty ? "" : " · \(t.preview)"),
                        open && dictations.open == nil, open || !locked)
            }
        }()
        Button {
            switch e {
            case .dictation(let d): dictations.openDictation(d.id)
            case .thread(let t):
                dictations.close()
                scribe.switchTo(t.id)
            }
        } label: {
            HStack(spacing: 12) {
                Text(icon).font(.title3)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.body.weight(selected ? .bold : .regular)).lineLimit(1)
                    Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
        }
        .disabled(!enabled || scribe.phase != .ready)
        .listRowBackground(selected ? Color.accentColor.opacity(0.15) : nil)
        .contextMenu {
            Button("Rename", systemImage: "pencil") { renaming(e) }
            let busyHere: Bool = { if case .thread(let t) = e { return t.id == scribe.thread.id && locked }; return false }()
            Button("Delete", systemImage: "trash", role: .destructive) { deleting(e) }
                .disabled(busyHere)
        }
    }
}

private struct Centered: View {
    let text: String
    let progress: Double?

    var body: some View {
        VStack(spacing: 16) {
            Text(text).multilineTextAlignment(.center)
            if let progress {
                ProgressView(value: progress)
            } else {
                ProgressView()
            }
        }
        .padding(24)
        .frame(maxWidth: 520)
    }
}

/// First run: what the models are, and the button to fetch them.
private struct Setup: View {
    @Environment(Scribe.self) private var scribe
    let error: String?
    let resume: Bool

    var body: some View {
        let mb = BUNDLES.map(\.approxBytes).reduce(0, +) / 1_000_000
        VStack(alignment: .leading, spacing: 16) {
            Text("Speech models").font(.title2.bold())
            Text("Scribe transcribes on this device. It needs two models from the sherpa-onnx releases on GitHub: Parakeet TDT 0.6B v2 (English) and a streaming Zipformer for live text. That's a one-time download of about \(mb) MB, kept in the app's storage. Wi-Fi recommended.")
            if let error {
                Text("Download failed: \(error)\nTrying again resumes where it stopped.").foregroundStyle(.red)
            }
            Button {
                scribe.install()
            } label: {
                Text(error != nil ? "Retry" : resume ? "Resume download" : "Download models").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
        }
        .padding(24)
        .frame(maxWidth: 560)
    }
}

/// One settings page.
struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(Scribe.self) private var scribe
    let page: SettingsPage

    var body: some View {
        @Bindable var scribe = scribe
        NavigationStack {
            Group {
                switch page {
                case .microphone:
                    Form {
                        Section("Mic") {
                            Picker("Mic", selection: $scribe.route) {
                                ForEach(MicRoute.allCases) { Text($0.label).tag($0) }
                            }
                            .pickerStyle(.inline)
                            .labelsHidden()
                        }
                        Section {
                            Picker("Mic boost", selection: $scribe.boost) {
                                ForEach(Boost.allCases) { Text($0.label).tag($0) }
                            }
                            .pickerStyle(.inline)
                            .labelsHidden()
                        } header: {
                            Text("Mic boost")
                        } footer: {
                            if scribe.recording { Text("Changes apply when the mic next starts (pause and resume).") }
                        }
                    }
                case .recording:
                    Form {
                        Toggle(isOn: $scribe.keepAudio) {
                            Text("Keep audio (.opus)")
                            Text("Save the audio with the transcript. Afterwards, tap a word to hear it from there.")
                        }
                        Toggle(isOn: $scribe.keepNonSpeech) {
                            Text("Keep non-speech audio")
                            Text("Also keep recordings in which no words were detected.")
                        }
                        .disabled(!scribe.keepAudio)
                        Toggle(isOn: $scribe.autoStart) {
                            Text("Start recording on New")
                            Text("A new dictation or transcription starts listening as soon as it opens.")
                        }
                    }
                case .display:
                    Form {
                        Section("Font size") {
                            Slider(value: Binding(get: { Double(scribe.fontStep) }, set: { scribe.fontStep = Int($0.rounded()) }),
                                   in: 0...Double(FONT_SCALES.count - 1), step: 1)
                            Text("The quick brown fox jumps over the lazy dog.")
                                .font(.system(size: UIFont.preferredFont(forTextStyle: .body).pointSize * FONT_SCALES[scribe.fontStep]))
                        }
                    }
                case .about:
                    AboutView()
                }
            }
            .navigationTitle(page.rawValue)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
    }
}

/// The app's name, version and source, then its changelog.
private struct AboutView: View {
    var body: some View {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                Text("Scribe").font(.largeTitle.bold())
                Text("Version \(version) (\(build))").foregroundStyle(.secondary)
                Text("Live transcription on this device, with the same speech models as scribe on the desktop.")
                Link("Source on GitHub", destination: URL(string: "https://github.com/EnigmaCurry/rpg_vox")!)
                Divider().padding(.vertical, 8)
                Changelog()
            }
            .padding(20)
            .frame(maxWidth: 820, alignment: .leading)
        }
    }
}

/// CHANGELOG.md, bundled with the app: headings, bullets and inline markdown.
private struct Changelog: View {
    var body: some View {
        let text = Bundle.main.url(forResource: "CHANGELOG", withExtension: "md")
            .flatMap { try? String(contentsOf: $0, encoding: .utf8) } ?? ""
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(blocks(text).enumerated()), id: \.offset) { _, b in
                line(b)
            }
        }
    }

    /// Lines joined into paragraphs and list items.
    private func blocks(_ text: String) -> [String] {
        var out: [String] = []
        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let l = String(raw)
            let t = l.trimmingCharacters(in: .whitespaces)
            if t.isEmpty {
                out.append("")
            } else if t.hasPrefix("#") || t.hasPrefix("- ") || out.isEmpty || out.last == "" {
                out.append(l)
            } else {
                out[out.count - 1] += " " + t
            }
        }
        return out.filter { !$0.isEmpty }
    }

    @ViewBuilder private func line(_ l: String) -> some View {
        let t = l.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("## ") {
            Text(t.dropFirst(3)).font(.title3.bold()).padding(.top, 12)
        } else if t.hasPrefix("### ") {
            Text(t.dropFirst(4)).font(.headline).padding(.top, 4)
        } else if t.hasPrefix("# ") {
            Text(t.dropFirst(2)).font(.title2.bold())
        } else if t.hasPrefix("- ") {
            let indent = CGFloat(l.prefix { $0 == " " }.count) * 8
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("•")
                Text(inline(String(t.dropFirst(2))))
            }
            .padding(.leading, indent)
        } else {
            Text(inline(t))
        }
    }

    private func inline(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
    }
}
