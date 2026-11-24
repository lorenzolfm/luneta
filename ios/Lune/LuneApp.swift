import SwiftUI

@main
struct LuneApp: App {
    @State private var luned = Luned()
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            HostView()
                .environment(luned)
        }
        .onChange(of: phase, initial: true) { _, now in
            if now == .active {
                luned.open()
            } else {
                luned.close()
            }
        }
    }
}
