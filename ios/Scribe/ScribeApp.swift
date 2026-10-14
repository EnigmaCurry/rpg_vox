import SwiftUI
import UIKit

/// Hands iOS's wake-up for the model download back to the download.
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication, handleEventsForBackgroundURLSession identifier: String,
                     completionHandler: @escaping () -> Void) {
        MainActor.assumeIsolated {
            if identifier == ModelStore.sessionId { Scribe.shared.models.backgroundDone = completionHandler }
        }
    }

    func applicationDidReceiveMemoryWarning(_ application: UIApplication) {
        // Free the models when nothing is using them; they reload on return.
        MainActor.assumeIsolated {
            if UIApplication.shared.applicationState != .active { Scribe.shared.release() }
        }
    }
}

@main
struct ScribeApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @Environment(\.scenePhase) private var scenePhase
    @State private var scribe = Scribe.shared
    @State private var dictations = Dictations.shared
    @State private var playback = Playback.shared

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(scribe)
                .environment(dictations)
                .environment(playback)
                // "Open in Scribe" / Copy to Scribe from the share sheet or Files.
                .onOpenURL { url in
                    dictations.close()
                    scribe.notice = scribe.transcribe(url)
                }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active:
                scribe.onVisible()
            case .background:
                // Recording and playback carry on (background audio); the
                // dictation screen stops listening, as on Android.
                dictations.stop()
                dictations.onHidden()
                scribe.sync()
            default:
                break
            }
        }
    }
}
