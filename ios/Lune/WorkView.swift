import LunePhone
import SwiftUI

struct WorkView: View {
    let machine: String?
    let project: Project
    @Environment(Luned.self) private var luned
    @Environment(\.dismiss) private var dismiss
    @State private var listed: Listed?
    @State private var failed: String?
    @State private var issues: Set<UInt32> = []
    @State private var pulls: Set<UInt32> = []
    @State private var tackling = false

    var body: some View {
        List {
            if let failed {
                Text(failed).foregroundStyle(.red)
            }
            switch listed {
            case .work(let open, let shownPulls, let morePulls)?:
                Section("Issues") {
                    switch open {
                    case .open(let shown, let more):
                        ForEach(shown, id: \.number) { issue in
                            Marking(
                                number: issue.number, title: issue.title,
                                detail: issue.labels.joined(separator: ", "), marked: $issues)
                        }
                        if shown.isEmpty {
                            Text("No open issues").foregroundStyle(.secondary)
                        }
                        if more {
                            Text("and more").foregroundStyle(.secondary)
                        }
                    case .off:
                        Text("The repository has issues off").foregroundStyle(.secondary)
                    }
                }
                Section("Pull requests") {
                    ForEach(shownPulls, id: \.number) { pull in
                        let author = pull.author.map { " by \($0)" } ?? ""
                        Marking(
                            number: pull.number, title: pull.title,
                            detail: "\(pull.draft ? "draft · " : "")\(pull.branch)\(author)",
                            marked: $pulls)
                    }
                    if shownPulls.isEmpty {
                        Text("No open pull requests").foregroundStyle(.secondary)
                    }
                    if morePulls {
                        Text("and more").foregroundStyle(.secondary)
                    }
                }
            case .unhosted?:
                Text("\(project.name) has no repository on GitHub").foregroundStyle(.secondary)
            case nil:
                if failed == nil {
                    ProgressView()
                }
            }
        }
        .navigationTitle("Tackle")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Tackle") { tackling = true }
                    .disabled(issues.isEmpty && pulls.isEmpty)
            }
        }
        .navigationDestination(isPresented: $tackling) {
            AskView(
                place: AskPlace(machine: machine, cwd: project.dir, title: project.name),
                opening: .tackle(
                    dir: project.dir,
                    marked: Marked(issues: issues.sorted(), pulls: pulls.sorted()),
                    said: marking))
        }
        .refreshable { await load() }
        .task { await load() }
    }

    private var marking: String {
        let named = issues.sorted().map { "#\($0)" } + pulls.sorted().map { "!\($0)" }
        return "Tackle \(named.joined(separator: ", "))"
    }

    private func load() async {
        do {
            listed = try await luned.work(on: machine, project: project.id)
            failed = nil
        } catch {
            failed = said(error)
        }
    }
}

private struct Marking: View {
    let number: UInt32
    let title: String
    let detail: String
    @Binding var marked: Set<UInt32>

    var body: some View {
        Button {
            if marked.contains(number) {
                marked.remove(number)
            } else {
                marked.insert(number)
            }
        } label: {
            HStack {
                Image(systemName: marked.contains(number) ? "checkmark.circle.fill" : "circle")
                    .accessibilityLabel(marked.contains(number) ? "Marked" : "Not marked")
                VStack(alignment: .leading) {
                    Text("#\(number) \(title)")
                    if !detail.isEmpty {
                        Text(detail).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .foregroundStyle(.primary)
    }
}
