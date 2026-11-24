import LunePhone
import SwiftUI

struct ProjectView: View {
    let seat: ProjectSeat
    @Environment(Luned.self) private var luned
    @State private var asking: AskPlace?
    @State private var startingAgent = false
    @State private var tackling = false
    @State private var closing: Worktree?
    @State private var closings: Set<UInt64> = []
    @State private var failed: String?

    var body: some View {
        let project = luned.project(at: seat)
        List {
            if let project {
                if let why = project.holds.unlisted {
                    Section {
                        Label(why, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                    }
                }
                ForEach(project.holds.worktrees, id: \.id) { worktree in
                    Section {
                        ForEach(worktree.views, id: \.id) { view in
                            ViewRow(view: view, machine: seat.machine, failed: $failed)
                        }
                        if worktree.views.isEmpty {
                            Text("No views").foregroundStyle(.secondary)
                        }
                    } header: {
                        WorktreeHeader(
                            worktree: worktree,
                            ask: {
                                asking = AskPlace(
                                    machine: seat.machine, cwd: worktree.folder,
                                    title: worktree.name)
                            },
                            closes: !closings.contains(worktree.id),
                            close: { closing = worktree })
                    }
                }
            } else {
                Text("luned no longer shows this project").foregroundStyle(.secondary)
            }
        }
        .navigationTitle(project?.name ?? "Project")
        .toolbar {
            if project != nil {
                Menu("Start", systemImage: "plus") {
                    Button("New agent", systemImage: "sparkles") { startingAgent = true }
                    Button("Tackle issues and pulls", systemImage: "checklist") { tackling = true }
                }
            }
        }
        .sheet(item: $asking) { place in
            NavigationStack { AskView(place: place, opening: nil) }
        }
        .sheet(isPresented: $startingAgent) {
            if let project {
                NavigationStack { NewAgentView(machine: seat.machine, project: project) }
            }
        }
        .sheet(isPresented: $tackling) {
            if let project {
                NavigationStack { WorkView(machine: seat.machine, project: project) }
            }
        }
        .confirmationDialog(
            "Close \(closing?.name ?? "the worktree")?", isPresented: closingShown,
            titleVisibility: .visible, presenting: closing
        ) { worktree in
            Button("Close the worktree and its views", role: .destructive) {
                let (closed, _) = closings.insert(worktree.id)
                precondition(closed)
                Task {
                    do {
                        try await luned.close(worktree, on: seat.machine)
                    } catch {
                        failed = reason(error)
                    }
                    closings.remove(worktree.id)
                }
            }
        }
        .alert("luned did not do it", isPresented: failedShown, presenting: failed) { _ in
            Button("OK") {}
        } message: { why in
            Text(why)
        }
    }

    private var closingShown: Binding<Bool> {
        Binding(get: { closing != nil }, set: { if !$0 { closing = nil } })
    }

    private var failedShown: Binding<Bool> {
        Binding(get: { failed != nil }, set: { if !$0 { failed = nil } })
    }
}

private struct WorktreeHeader: View {
    let worktree: Worktree
    let ask: () -> Void
    let closes: Bool
    let close: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading) {
                Text(worktree.name)
                let caption = [worktree.branch, worktree.seen.words].compactMap { $0 }
                if !caption.isEmpty {
                    Text(caption.joined(separator: " · ")).font(.caption2).textCase(nil)
                }
            }
            Spacer()
            Menu {
                Button("Ask here", systemImage: "bubble.left", action: ask)
                Button("Close worktree", systemImage: "xmark", role: .destructive, action: close)
                    .disabled(!closes)
            } label: {
                Image(systemName: "ellipsis.circle")
                    .accessibilityLabel("Worktree actions")
            }
        }
    }
}

private struct ViewRow: View {
    let view: LuneView
    let machine: String?
    @Binding var failed: String?
    @Environment(Luned.self) private var luned
    @State private var closing = false

    var body: some View {
        let agent = view.terminal.flatMap { luned.agent(at: Seat(machine: machine, terminal: $0)) }
        Group {
            if let agent, let seat = agent.seat {
                NavigationLink(value: seat) {
                    row(status: agent.status.words, waits: agent.waits)
                }
            } else {
                row(status: view.command, waits: false)
            }
        }
        .swipeActions {
            if let terminal = view.terminal {
                Button("Close", role: .destructive) {
                    precondition(!closing)
                    closing = true
                    Task {
                        do {
                            try await luned.close(terminal, on: machine)
                        } catch {
                            failed = reason(error)
                        }
                        closing = false
                    }
                }
                .disabled(closing)
            }
        }
    }

    private func row(status: String?, waits: Bool) -> some View {
        VStack(alignment: .leading) {
            Text(view.title).lineLimit(1)
            if let status {
                Text(status)
                    .font(.caption)
                    .foregroundStyle(waits ? .orange : .secondary)
                    .lineLimit(1)
            }
        }
    }
}
