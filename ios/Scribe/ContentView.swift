import SwiftUI
import UIKit

struct ContentView: View {
    @EnvironmentObject var scribe: Scribe

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                transcript
                Divider()
                controls
            }
            .navigationTitle("Scribe")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    Button {
                        UIPasteboard.general.string = scribe.text
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                    .disabled(scribe.text.isEmpty)
                    ShareLink(item: scribe.text) {
                        Label("Share", systemImage: "square.and.arrow.up")
                    }
                    .disabled(scribe.text.isEmpty)
                }
            }
        }
    }

    private var transcript: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    ForEach(scribe.paragraphs) { p in
                        paragraph(p).id(p.id)
                    }
                    Color.clear.frame(height: 1).id("end")
                }
                .padding()
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .onChange(of: scribe.paragraphs.last?.text) {
                withAnimation { proxy.scrollTo("end", anchor: .bottom) }
            }
            .overlay { placeholder }
        }
    }

    /// Final clips in the body color, pass-1 partials dimmed.
    private func paragraph(_ p: Paragraph) -> some View {
        p.clips.enumerated().reduce(Text("")) { acc, e in
            let t = Text((e.offset == 0 ? "" : " ") + e.element.text)
            return acc + (e.element.partial ? t.foregroundColor(.secondary) : t)
        }
        .font(.title3)
        .textSelection(.enabled)
    }

    @ViewBuilder private var placeholder: some View {
        switch scribe.phase {
        case .loading:
            ProgressView("Loading models…")
        case .noModels(let why), .failed(let why):
            VStack(spacing: 12) {
                Text(why).multilineTextAlignment(.center)
                Button("Retry") { scribe.load() }
            }
            .padding()
        case .idle where scribe.paragraphs.isEmpty:
            Text("Tap the microphone to dictate.").foregroundStyle(.secondary)
        default:
            EmptyView()
        }
    }

    private var controls: some View {
        VStack(spacing: 8) {
            HStack(spacing: 24) {
                Button {
                    scribe.newParagraph()
                } label: {
                    Image(systemName: "return").font(.title2)
                }
                .disabled(scribe.phase != .recording)

                Button {
                    scribe.phase == .recording ? scribe.stop() : scribe.start()
                } label: {
                    Image(systemName: scribe.phase == .recording ? "stop.circle.fill" : "mic.circle.fill")
                        .font(.system(size: 64))
                        .foregroundStyle(scribe.phase == .recording ? .red : .accentColor)
                }
                .disabled(scribe.phase != .idle && scribe.phase != .recording)

                level.frame(width: 28, height: 44)
            }
            if scribe.phase == .finishing {
                ProgressView("Finishing…")
            }
            Text(scribe.stats)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding()
    }

    /// Mic level, green while the voice detector hears speech.
    private var level: some View {
        GeometryReader { g in
            let db = 20 * log10(max(Double(scribe.rms), 1e-4))
            let frac = min(max((db + 60) / 60, 0), 1)
            ZStack(alignment: .bottom) {
                Capsule().fill(Color.secondary.opacity(0.2))
                Capsule()
                    .fill(scribe.speaking ? Color.green : Color.secondary)
                    .frame(height: g.size.height * frac)
            }
        }
        .opacity(scribe.phase == .recording ? 1 : 0.3)
    }
}
