import LunePhone
import SwiftUI

struct InboxView: View {
    @Environment(Luned.self) private var luned
    @State private var naming = false

    var body: some View {
        NavigationStack {
            Group {
                switch luned.reach {
                case .unnamed:
                    MachineView()
                case .refused(_, let why), .watching(_, _, .lost(let why)):
                    ContentUnavailableView(
                        "luned is out of reach", systemImage: "wifi.exclamationmark",
                        description: Text(why))
                case .watching(let host, _, .nothing):
                    ProgressView("Reaching \(host)")
                case .watching(_, _, .model(let model, let lost)):
                    InboxList(model: model, lost: lost)
                }
            }
            .navigationTitle("Inbox")
            .navigationDestination(for: ProjectSeat.self) { ProjectView(seat: $0) }
            .navigationDestination(for: TerminalKey.self) { AgentView(terminal: $0) }
            .navigationDestination(for: TerminalPlace.self) { TerminalPage(place: $0) }
            .toolbar {
                if luned.reach.host != nil {
                    Button("Machine", systemImage: "desktopcomputer") { naming = true }
                }
            }
            .sheet(isPresented: $naming) {
                NavigationStack { MachineView() }
            }
        }
    }
}

private struct FlowAlert: Hashable {
    let seat: ProjectSeat
    let projectName: String
    let worktreeName: String
    let flow: Flow
}

private func flowAlerts(in model: Model) -> [FlowAlert] {
    var alerts: [FlowAlert] = []
    func collect(machine: String?, held: Held) {
        guard case .kept(let projects) = held else { return }
        for project in projects {
            let seat = ProjectSeat(machine: machine, project: project.id)
            for worktree in project.holds.worktrees {
                if let flow = worktree.flow, flow.needsAttention {
                    alerts.append(
                        FlowAlert(
                            seat: seat, projectName: project.name, worktreeName: worktree.name,
                            flow: flow))
                }
            }
        }
    }
    collect(machine: nil, held: model.here)
    for far in model.far {
        collect(machine: far.on, held: far.held)
    }
    return alerts
}

private struct InboxList: View {
    let model: Model
    let lost: String?

    var body: some View {
        let flows = flowAlerts(in: model)
        let waiting = model.inbox.filter(\.waits)
        let others = model.inbox.filter { !$0.waits }
        List {
            if let lost {
                Section {
                    Label(lost, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                }
            }
            if !flows.isEmpty {
                Section("Flows") {
                    ForEach(flows, id: \.self) { alert in
                        NavigationLink(value: alert.seat) {
                            VStack(alignment: .leading, spacing: 2) {
                                HStack {
                                    Text("\(alert.projectName) › \(alert.worktreeName)")
                                        .font(.headline)
                                        .lineLimit(1)
                                    Spacer()
                                    Text(alert.flow.badge)
                                        .font(.caption.weight(.semibold))
                                        .foregroundStyle(
                                            alert.flow.badgeKind == .paused ? .red : .orange)
                                }
                                if let detail = alert.flow.detail {
                                    Text(detail)
                                        .font(.subheadline)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(2)
                                }
                            }
                        }
                    }
                }
            }
            Section("Waiting") {
                if waiting.isEmpty {
                    Text("No agent waits for you").foregroundStyle(.secondary)
                }
                ForEach(waiting, id: \.self) { AgentRow(agent: $0) }
            }
            if !others.isEmpty {
                Section("The rest") {
                    ForEach(others, id: \.self) { AgentRow(agent: $0) }
                }
            }
        }
    }
}

private struct AgentRow: View {
    let agent: Agent

    var body: some View {
        if let terminal = agent.terminal {
            NavigationLink(value: terminal) { row }
        } else {
            row
        }
    }

    private var row: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text(agent.name).font(.headline).lineLimit(1)
                Spacer()
                if let since = agent.since {
                    Text(since, style: .relative)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
            }
            if let place = agent.place {
                Text(place).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
            }
            Text(agent.status.words)
                .font(.subheadline)
                .foregroundStyle(agent.waits ? .orange : .secondary)
        }
    }
}
