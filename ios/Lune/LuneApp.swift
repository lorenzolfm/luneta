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
                Tab("Tree", systemImage: "list.bullet.indent") {
                    TreeView()
                }
                Tab("Ask", systemImage: "bubble.left.and.text.bubble.right") {
                    NavigationStack {
                        AskView(
                            place: AskPlace(machine: nil, cwd: nil, title: "Ask"), opening: nil
                        )
                        .id(luned.reach.host)
                    }
                }
            }
            .environment(luned)
        }
        .onChange(of: phase, initial: true) { _, now in
            if now == .active {
                luned.watch()
            } else {
                luned.unwatch()
            }
        }
    }
}
