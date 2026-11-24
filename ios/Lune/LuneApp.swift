import LunePhone
import SwiftUI

@main
struct LuneApp: App {
    @State private var luned = Luned()
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            TabView {
                Tab("Inbox", systemImage: "tray") {
                    InboxView()
                }
                .badge(luned.model?.inbox.filter(\.waits).count ?? 0)
            }
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
