import LunePhone
import SwiftUI

struct TreeView: View {
    @Environment(Luned.self) private var luned

    var body: some View {
        NavigationStack {
            Group {
                if case .watching(let host, _, .model(let model, let lost)) = luned.reach {
                    List {
                        if let lost {
                            Section {
                                Label(lost, systemImage: "exclamationmark.triangle")
                                    .foregroundStyle(.red)
                            }
                        }
                        MachineSection(
                            name: host, machine: nil, reached: nil, held: model.here)
                        ForEach(model.far, id: \.on) { far in
                            MachineSection(
                                name: far.on, machine: far.on, reached: far.reached, held: far.held)
                        }
                    }
                } else {
                    ContentUnavailableView(
                        "No tree yet", systemImage: "folder",
                        description: Text(luned.reach.trouble ?? "luned has said nothing yet"))
                }
            }
            .navigationTitle("Tree")
            .navigationDestination(for: ProjectSeat.self) { ProjectView(seat: $0) }
            .navigationDestination(for: Seat.self) { AgentView(seat: $0) }
        }
    }
}

private struct MachineSection: View {
    let name: String
    let machine: String?
    let reached: Reached?
    let held: Held

    var body: some View {
        Section {
            switch reached {
            case .reaching?:
                Text("Reaching…").foregroundStyle(.secondary)
            case .unreached(let why, _)?:
                Text(why).foregroundStyle(.red)
            case .reached?, nil:
                EmptyView()
            }
            switch held {
            case .kept(let projects):
                ForEach(projects, id: \.id) { project in
                    NavigationLink(value: ProjectSeat(machine: machine, project: project.id)) {
                        VStack(alignment: .leading) {
                            Text(project.name)
                            Text(project.missing ? "missing" : project.dir)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.head)
                        }
                    }
                }
            case .unkept(let why):
                Text(why).foregroundStyle(.secondary)
            case .untold:
                Text("No projects told").foregroundStyle(.secondary)
            }
        } header: {
            Label(name, systemImage: "desktopcomputer")
        }
    }
}
