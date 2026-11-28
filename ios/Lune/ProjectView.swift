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
    @State private var flowBusy: Set<UInt64> = []
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
                            ViewRow(view: view, failed: $failed)
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
                            flowEnabled: !flowBusy.contains(worktree.id),
                            startFlow: { interactive in
                                let (inserted, _) = flowBusy.insert(worktree.id)
                                precondition(inserted)
                                Task {
                                    do {
                                        try await luned.startFlow(
                                            on: seat.machine, worktree: worktree.id,
                                            interactive: interactive)
                                    } catch {
                                        failed = reason(error)
                                    }
                                    flowBusy.remove(worktree.id)
                                }
                            },
                            controlFlow: { control in
                                let (inserted, _) = flowBusy.insert(worktree.id)
                                precondition(inserted)
                                Task {
                                    do {
                                        try await luned.controlFlow(
                                            on: seat.machine, worktree: worktree.id,
                                            control: control)
                                    } catch {
                                        failed = reason(error)
                                    }
                                    flowBusy.remove(worktree.id)
                                }
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
    let flowEnabled: Bool
    let startFlow: (Bool) -> Void
    let controlFlow: (FlowControl) -> Void
    let closes: Bool
    let close: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(worktree.name)
                    if let flow = worktree.flow {
                        FlowBadge(flow: flow)
                    }
                }
                let caption = [worktree.branch, worktree.seen.words].compactMap { $0 }
                if !caption.isEmpty {
                    Text(caption.joined(separator: " · ")).font(.caption2).textCase(nil)
                }
                if let flow = worktree.flow,
                    flow.badgeKind == .paused || flow.badgeKind == .confirmMerge,
                    let detail = flow.detail
                {
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(flow.badgeKind == .paused ? .red : .green)
                        .textCase(nil)
                        .lineLimit(2)
                }
            }
            Spacer()
            Menu {
                if let flow = worktree.flow, !flow.controls.isEmpty {
                    Section("Flow") {
                        ForEach(flow.controls, id: \.self) { control in
                            Button(
                                control.title,
                                systemImage: control.systemImage,
                                role: control.isDestructive ? .destructive : nil
                            ) {
                                controlFlow(control)
                            }
                            .disabled(!flowEnabled)
                        }
                    }
                }
                if worktree.flow == nil || worktree.flow?.canStartNew == true {
                    Section("Flow") {
                        Button("Start PR flow (auto)", systemImage: "bolt") {
                            startFlow(false)
                        }
                        .disabled(!flowEnabled)
                        Button("Start PR flow (interactive)", systemImage: "play.circle") {
                            startFlow(true)
                        }
                        .disabled(!flowEnabled)
                    }
                }
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

private struct FlowBadge: View {
    let flow: Flow

    var body: some View {
        Text(flow.badge)
            .font(.caption2.weight(.semibold))
            .textCase(nil)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .foregroundStyle(tint)
            .background(tint.opacity(0.16), in: Capsule())
    }

    private var tint: Color {
        switch flow.badgeKind {
        case .running: .blue
        case .waitingInteractive: .orange
        case .confirmMerge: .green
        case .paused: .red
        case .merged: .purple
        case .readyToMerge: .green
        }
    }
}

private struct ViewRow: View {
    let view: LuneView
    @Binding var failed: String?
    @Environment(Luned.self) private var luned
    @State private var closing = false

    var body: some View {
        let agent = view.key.flatMap { luned.agent(at: $0) }
        Group {
            if let agent, let terminal = agent.terminal {
                NavigationLink(value: terminal) {
                    row(status: agent.status.words, waits: agent.waits)
                }
            } else if let terminal = view.key {
                NavigationLink(value: TerminalPlace(terminal: terminal, title: view.title)) {
                    row(status: view.found.words ?? view.command, waits: false)
                }
            } else {
                row(status: view.found.words ?? view.command, waits: false)
            }
        }
        .swipeActions {
            if let terminal = view.key {
                Button("Close", role: .destructive) {
                    precondition(!closing)
                    closing = true
                    Task {
                        do {
                            try await luned.close(terminal)
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
