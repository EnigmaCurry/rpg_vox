import SwiftUI

@main
struct ScribeApp: App {
    @StateObject private var scribe = Scribe()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(scribe)
                .onAppear { scribe.load() }
        }
    }
}
