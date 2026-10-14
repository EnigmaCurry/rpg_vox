import SwiftUI

private let RED = Color(red: 0.83, green: 0.18, blue: 0.18)
private let GREEN = Color(red: 0.5, green: 0.82, blue: 0.63)

/// The controls along the bottom of every screen (dictation,
/// transcription, playback), as on Android: one row, four round buttons
/// mirrored around a big one in the middle, under a short status line. A
/// slot left empty keeps its space, so nothing shifts as buttons come
/// and go.
struct Dock<L2: View, L1: View, Center: View, R1: View, R2: View>: View {
    let status: String
    let live: Bool
    @ViewBuilder var l2: L2
    @ViewBuilder var l1: L1
    @ViewBuilder var center: Center
    @ViewBuilder var r1: R1
    @ViewBuilder var r2: R2

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 6) {
                if live {
                    Circle().fill(RED).frame(width: 8, height: 8)
                }
                Text(status)
                    .font(.subheadline.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            HStack {
                Spacer()
                slot { l2 }
                Spacer()
                slot { l1 }
                Spacer()
                center
                Spacer()
                slot { r1 }
                Spacer()
                slot { r2 }
                Spacer()
            }
            .frame(maxWidth: 520)
        }
        .padding(.horizontal, 16)
        .padding(.top, 10)
        .padding(.bottom, 6)
        .frame(maxWidth: .infinity)
        .background(.bar)
    }

    /// Keeps its place when empty (EmptyView alone takes no room).
    private func slot<V: View>(@ViewBuilder _ v: () -> V) -> some View {
        ZStack {
            Color.clear
            v()
        }
        .frame(width: 52, height: 52)
    }
}

/// A round button in the dock: `icon` (an SF Symbol), described as `label` for VoiceOver.
struct DockButton: View {
    let icon: String
    let label: String
    var enabled = true
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.title3)
                .frame(width: 52, height: 52)
                .background(Circle().fill(Color(.secondarySystemFill)))
        }
        .buttonStyle(.plain)
        .foregroundStyle(enabled ? Color.primary : Color.secondary.opacity(0.5))
        .disabled(!enabled)
        .accessibilityLabel(label)
    }
}

/// The big button in the middle: record (a red dot) or stop (a white
/// square on red), or with `icon` something else (play, pause). The ring
/// around it shows `ring` (0 to 1): the mic's level, or progress.
struct BigButton: View {
    let label: String
    var enabled = true
    var recording = false
    var icon: String? = nil
    var ring: Double = 0
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                Circle().stroke(Color(.systemFill), lineWidth: 4)
                if ring > 0 {
                    Circle()
                        .trim(from: 0, to: min(max(ring, 0), 1))
                        .stroke(GREEN, style: StrokeStyle(lineWidth: 4, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                        .animation(.linear(duration: 0.1), value: ring)
                }
                Circle()
                    .fill(fill)
                    .frame(width: 66, height: 66)
                if let icon {
                    Image(systemName: icon).font(.system(size: 28, weight: .semibold)).foregroundStyle(.white)
                } else if recording {
                    RoundedRectangle(cornerRadius: 4).fill(.white).frame(width: 22, height: 22)
                } else {
                    Circle().fill(enabled ? RED : RED.opacity(0.4)).frame(width: 24, height: 24)
                }
            }
            .frame(width: 80, height: 80)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityLabel(label)
    }

    private var fill: Color {
        if !enabled { return Color(.systemFill) }
        if icon != nil { return .accentColor }
        if recording { return RED }
        return Color(.systemFill)
    }
}
