import SwiftUI

/// The open dictation: its text, with the record button and the rest below.
struct DictationScreen: View {
    @Environment(Dictations.self) private var dictations
    @Environment(Scribe.self) private var scribe

    var body: some View {
        let typing = dictations.typing
        let hasText = typing.map { !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } ?? !dictations.laid.toks.isEmpty
        VStack(spacing: 0) {
            if let typing {
                TypingEditor(typing: typing, fontStep: scribe.fontStep)
            } else {
                ZStack(alignment: .top) {
                    DictationEditor(laid: dictations.laid, held: dictations.held, selection: dictations.selection,
                                    fontStep: scribe.fontStep, listening: dictations.listening)
                    if dictations.laid.toks.isEmpty && !dictations.listening {
                        Text("Dictate text you can keep editing: tap the text to move the cursor, where new speech goes, and select words to delete them. ¶ starts a new paragraph, ⌫ deletes the last sentence, and the keyboard button opens your keyboard to edit the text by hand.")
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 16)
                            .padding(.top, 64)
                            .allowsHitTesting(false)
                    }
                }
            }
            // Out of the way while typing; Done comes back to them.
            if typing == nil {
                Dock(
                    status: dictations.finishing ? "Finishing…" : dictations.listening ? "Listening"
                        : hasText ? "Tap ● to dictate at the cursor" : "Tap ● to dictate",
                    live: dictations.listening,
                    // A new paragraph at the cursor.
                    l2: { DockButton(icon: "paragraphsign", label: "New paragraph", enabled: hasText) { dictations.paragraphBreak() } },
                    // The selection, or else the sentence before the cursor.
                    l1: {
                        DockButton(icon: "delete.left", label: dictations.selection.length == 0 ? "Delete the last sentence" : "Delete the selection",
                                   enabled: hasText) { dictations.deleteSentence() }
                    },
                    center: {
                        BigButton(label: dictations.listening ? "Stop dictating" : "Dictate", enabled: !dictations.finishing,
                                  recording: dictations.listening, ring: Double(dictations.level) * 8, action: toggle)
                    },
                    // Type at the cursor; Done comes back here.
                    r1: { DockButton(icon: "keyboard", label: "Type") { dictations.typeMode() } },
                    r2: {
                        DockButton(icon: "doc.on.doc", label: "Copy", enabled: hasText) {
                            UIPasteboard.general.string = dictations.text()
                        }
                    }
                )
            }
        }
        .toolbar {
            if typing != nil {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dictations.dictateMode() }
                }
            }
        }
        .task(id: "\(scribe.startWhenOpen ?? "")\(dictations.open?.id ?? 0)\(dictations.finishing)") {
            // Made with New, with "Start recording on New" on: once the last one has finished.
            guard scribe.startWhenOpen == "dictation", dictations.open != nil, !dictations.finishing else { return }
            scribe.startWhenOpen = nil
            if !dictations.listening { toggle() }
        }
    }

    private func toggle() {
        if dictations.listening {
            dictations.stop()
        } else {
            withMic({ dictations.notice = dictations.start() }, denied: { dictations.notice = MIC_DENIED })
        }
    }
}
