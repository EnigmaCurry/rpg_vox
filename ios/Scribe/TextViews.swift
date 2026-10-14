import SwiftUI
import UIKit

/// Transcript text sizes the Display setting steps through; the middle one is the default.
let FONT_SCALES: [CGFloat] = [0.8, 0.9, 1, 1.15, 1.3]

/// The transcript's font at the chosen size.
@MainActor
func transcriptFont() -> UIFont {
    let base = UIFont.preferredFont(forTextStyle: .body)
    return base.withSize(base.pointSize * FONT_SCALES[Scribe.shared.fontStep])
}

/// A read-only, selectable UITextView sized to its text, for text that
/// SwiftUI's Text can't do: taps on words, and items of our own in the
/// selection menu.
class FitTextView: UITextView {
    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        isEditable = false
        isSelectable = true
        isScrollEnabled = false
        backgroundColor = .clear
        textContainerInset = .zero
        self.textContainer.lineFragmentPadding = 0
        setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    }

    required init?(coder: NSCoder) { fatalError() }

    /// UTF-16 offset of the character nearest `point`.
    func offset(at point: CGPoint) -> Int {
        guard let pos = closestPosition(to: point) else { return 0 }
        return self.offset(from: beginningOfDocument, to: pos)
    }
}

/// The word being heard: in paragraph `para`, word `word`, which starts at `ms` on the timeline.
struct Karaoke: Equatable {
    let para: String
    let word: Int
    let ms: Int64
}

/// A finished paragraph: tap a word to play from it (when it has kept
/// audio), long-press selects text, whose menu adds Edit and Cut. While
/// its recording plays, the words already heard are lit and the current
/// one highlighted.
struct ParagraphText: UIViewRepresentable {
    let para: Para
    let playing: Playing?
    let at: Karaoke?
    let canPlay: Bool
    let busy: Bool
    let fontStep: Int
    var onEdit: (Para) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> FitTextView {
        let v = FitTextView()
        v.delegate = context.coordinator
        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.tapped(_:)))
        tap.delegate = context.coordinator
        v.addGestureRecognizer(tap)
        return v
    }

    func updateUIView(_ v: FitTextView, context: Context) {
        context.coordinator.view = self
        let text = attributed()
        if v.attributedText != text {
            let sel = v.selectedRange
            v.attributedText = text
            if sel.upperBound <= text.length { v.selectedRange = sel }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: FitTextView, context: Context) -> CGSize? {
        let w = proposal.width ?? UIScreen.main.bounds.width
        let size = uiView.sizeThatFits(CGSize(width: w, height: .greatestFiniteMagnitude))
        return CGSize(width: w, height: ceil(size.height))
    }

    private func attributed() -> NSAttributedString {
        let shown = para.shown
        let font = transcriptFont()
        let out = NSMutableAttributedString()
        let off = playing?.offsets[para.audio ?? ""]
        for (i, w) in shown.enumerated() {
            if i > 0 { out.append(NSAttributedString(string: " ", attributes: [.font: font, .foregroundColor: UIColor.label])) }
            var attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.label]
            if let at, let off {
                let t = off + para.wordAt(i, shown.count).startMs
                if at.para == para.id && at.word == i {
                    attrs[.backgroundColor] = UIColor.tintColor.withAlphaComponent(0.25)
                } else if t < at.ms {
                    attrs[.foregroundColor] = UIColor.tintColor
                }
            }
            out.append(NSAttributedString(string: w, attributes: attrs))
        }
        return out
    }

    /// Start offset (UTF-16) of each shown word.
    fileprivate func starts() -> [Int] {
        var at = 0
        return para.shown.map { w in
            defer { at += w.utf16.count + 1 }
            return at
        }
    }

    final class Coordinator: NSObject, UITextViewDelegate, UIGestureRecognizerDelegate {
        var view: ParagraphText?

        @MainActor @objc func tapped(_ g: UITapGestureRecognizer) {
            guard let view, let v = g.view as? FitTextView else { return }
            let pb = Playback.shared
            // While playing, a tap pauses instead (as the transcript does).
            if pb.playing != nil && !pb.paused {
                pb.pause()
                return
            }
            guard view.canPlay, v.selectedRange.length == 0 else { return }
            let c = v.offset(at: g.location(in: v))
            let starts = view.starts()
            let i = max((starts.lastIndex { $0 <= c }) ?? 0, 0)
            Scribe.shared.play(view.para, view.para.wordAt(i, starts.count))
        }

        func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }

        @MainActor
        func textView(_ textView: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
            guard let view, range.length > 0 else { return UIMenu(children: suggestedActions) }
            var mine: [UIMenuElement] = []
            // Edits would be overwritten while the engine still works on the
            // text; cuts are reapplied to its later versions.
            if !view.busy {
                mine.append(UIAction(title: "Edit") { _ in view.onEdit(view.para) })
            }
            mine.append(UIAction(title: "Cut", attributes: .destructive) { _ in
                Scribe.shared.cut(view.para, from: range.location, to: range.upperBound)
                textView.selectedRange = NSRange(location: 0, length: 0)
            })
            return UIMenu(children: suggestedActions + [UIMenu(options: .displayInline, children: mine)])
        }
    }
}

/// The open dictation's text, as one block: a tap moves the purple
/// cursor, where new speech goes, to the nearest gap between words (below
/// the text: to the end); selected words (and the breaks between
/// paragraphs) can be deleted from the selection menu.
struct DictationEditor: UIViewRepresentable {
    let laid: Laid
    let held: Laid?
    let selection: NSRange
    let fontStep: Int
    let listening: Bool

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.isEditable = false
        v.isSelectable = true
        v.alwaysBounceVertical = true
        v.backgroundColor = .clear
        v.textContainerInset = UIEdgeInsets(top: 16, left: 16, bottom: 120, right: 16)
        v.delegate = context.coordinator
        let caret = UIView()
        caret.backgroundColor = UIColor { $0.userInterfaceStyle == .dark
            ? UIColor(red: 0.81, green: 0.58, blue: 0.85, alpha: 1)
            : UIColor(red: 0.56, green: 0.14, blue: 0.67, alpha: 1) }
        caret.isUserInteractionEnabled = false
        v.addSubview(caret)
        context.coordinator.caret = caret
        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.tapped(_:)))
        tap.delegate = context.coordinator
        v.addGestureRecognizer(tap)
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {
        let c = context.coordinator
        c.editor = self
        let shown = held ?? laid
        let font = transcriptFont()
        let text = NSMutableAttributedString(string: shown.text, attributes: [.font: font, .foregroundColor: UIColor.label])
        for (r, t) in shown.toks where t.partial {
            text.addAttributes([.foregroundColor: UIColor.secondaryLabel, .font: italic(font)],
                               range: NSRange(location: r.lowerBound, length: r.count))
        }
        if v.attributedText != text {
            c.updating = true
            v.attributedText = text
            if selection.length > 0 && selection.upperBound <= text.length { v.selectedRange = selection }
            c.updating = false
        }
        if selection.length == 0 && v.selectedRange.length > 0 {
            c.updating = true
            v.selectedRange = NSRange(location: 0, length: 0)
            c.updating = false
        }
        c.placeCaret(v, shown.caret)
    }

    private func italic(_ f: UIFont) -> UIFont {
        f.fontDescriptor.withSymbolicTraits(.traitItalic).map { UIFont(descriptor: $0, size: f.pointSize) } ?? f
    }

    final class Coordinator: NSObject, UITextViewDelegate, UIGestureRecognizerDelegate {
        var editor: DictationEditor?
        var caret: UIView?
        var updating = false
        /// Keep the cursor in view, until the reader scrolls away from it.
        var follow = true

        @MainActor
        func placeCaret(_ v: UITextView, _ offset: Int) {
            guard let caret else { return }
            v.layoutIfNeeded()
            guard offset >= 0, offset <= (v.text as NSString).length,
                  let pos = v.position(from: v.beginningOfDocument, offset: offset) else {
                caret.isHidden = true
                return
            }
            let r = v.caretRect(for: pos)
            caret.isHidden = r.isNull || r.isInfinite
            caret.frame = CGRect(x: r.minX, y: r.minY, width: 2, height: r.height)
            if follow && editor?.held == nil {
                // With a line's room below it.
                v.scrollRectToVisible(r.insetBy(dx: 0, dy: -r.height), animated: false)
            }
        }

        @MainActor @objc func tapped(_ g: UITapGestureRecognizer) {
            guard let v = g.view as? UITextView, v.selectedRange.length == 0 else { return }
            follow = true
            let p = g.location(in: v)
            let end = v.layoutManager.usedRect(for: v.textContainer).maxY + v.textContainerInset.top
            if p.y > end {
                Dictations.shared.moveCursor((Dictations.shared.laid.text as NSString).length)
            } else if let pos = v.closestPosition(to: p) {
                Dictations.shared.moveCursor(v.offset(from: v.beginningOfDocument, to: pos))
            }
        }

        func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }

        func scrollViewWillBeginDragging(_ scrollView: UIScrollView) { follow = false }

        @MainActor
        func textViewDidChangeSelection(_ v: UITextView) {
            if updating { return }
            Dictations.shared.select(v.selectedRange)
        }

        @MainActor
        func textView(_ textView: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
            let d = Dictations.shared
            let picked = d.picked(d.held ?? d.laid, range)
            guard range.length > 0, !picked.isEmpty else { return UIMenu(children: suggestedActions) }
            let delete = UIAction(title: "Delete", attributes: .destructive) { _ in d.delete(picked) }
            return UIMenu(children: suggestedActions + [UIMenu(options: .displayInline, children: [delete])])
        }
    }
}

/// Typing mode: the dictation as an ordinary text field, with the
/// keyboard up. Leaving it (the keyboard's dismiss key, or Done) goes
/// back to dictating.
struct TypingEditor: UIViewRepresentable {
    let typing: Typing
    let fontStep: Int

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.backgroundColor = .clear
        v.textContainerInset = UIEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        v.autocapitalizationType = .sentences
        v.delegate = context.coordinator
        v.text = typing.text
        v.font = transcriptFont()
        v.selectedRange = typing.selection
        DispatchQueue.main.async {
            v.becomeFirstResponder()
            v.scrollRangeToVisible(v.selectedRange)
        }
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {
        v.font = transcriptFont()
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        @MainActor
        func textViewDidChange(_ v: UITextView) {
            Dictations.shared.type(Typing(text: v.text, selection: v.selectedRange))
        }

        @MainActor
        func textViewDidChangeSelection(_ v: UITextView) {
            Dictations.shared.type(Typing(text: v.text, selection: v.selectedRange))
        }

        @MainActor
        func textViewDidEndEditing(_ v: UITextView) {
            Dictations.shared.dictateMode()
        }
    }
}
