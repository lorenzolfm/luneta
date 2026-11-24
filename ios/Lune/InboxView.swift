import LunePhone
import SwiftUI

struct InboxView: View {
    @Environment(Luned.self) private var luned
    @State private var naming = false

    var body: some View {
        NavigationStack {
            Group {
                if luned.host.isEmpty {
                    MachineView()
                } else if let model = luned.model {
                    InboxList(model: model, lost: luned.lost)
                } else if let why = luned.refused ?? luned.lost {
                    ContentUnavailableView(
                        "luned is out of reach", systemImage: "wifi.exclamationmark",
                        description: Text(why))
                } else {
                    ProgressView("Reaching \(luned.host)")
                }
            }
            .navigationTitle("Inbox")
            .navigationDestination(for: Seat.self) { AgentView(seat: $0) }
            .toolbar {
                if !luned.host.isEmpty {
                    Button("Machine", systemImage: "desktopcomputer") { naming = true }
                }
            }
            .sheet(isPresented: $naming) {
                NavigationStack { MachineView() }
            }
        }
    }
}

private struct InboxList: View {
    let model: Model
    let lost: String?

    var body: some View {
        let waiting = model.inbox.filter(\.waits)
        let others = model.inbox.filter { !$0.waits }
        List {
            if let lost {
                Section {
                    Label(lost, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
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
        if let seat = agent.seat {
            NavigationLink(value: seat) { row }
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
