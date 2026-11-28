import LunePhone
import SwiftUI

struct WorkView: View {
    let machine: String?
    let project: Project
    @Environment(Luned.self) private var luned
    @Environment(\.dismiss) private var dismiss
    @State private var listing: Listing?
    @State private var failed: String?
    @State private var tackling: Marked?

    var body: some View {
        List {
            if let failed {
                Text(failed).foregroundStyle(.red)
            }
            switch listing?.listed {
            case .work(let open, let shownPulls, let morePulls)?:
                Section("Issues") {
                    switch open {
                    case .open(let shown, let more):
                        ForEach(shown, id: \.number) { issue in
                            Marking(
                                number: issue.number, title: issue.title,
                                detail: issue.labels.joined(separator: ", "),
                                marked: marks(\.issues))
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
                        let detail = [
                            pull.mergeability.words,
                            pull.draft ? "draft" : nil,
                            "\(pull.branch)\(author)",
                        ].compactMap { $0 }.joined(separator: " · ")
                        Marking(
                            number: pull.number, title: pull.title,
                            detail: detail,
                            marked: marks(\.pulls))
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
                Button("Tackle") { tackling = listing?.marked }
                    .disabled(listing?.marks != true)
            }
        }
        .navigationDestination(item: $tackling) { marked in
            AskView(
                place: AskPlace(machine: machine, cwd: project.dir, title: project.name),
                opening: .tackle(marked: marked, asked: marked.asked))
        }
        .refreshable { await load() }
        .task { await load() }
    }

    private func marks(_ kind: WritableKeyPath<Listing, Set<UInt32>>) -> Binding<Set<UInt32>> {
        Binding(
            get: { listing?[keyPath: kind] ?? [] },
            set: { listing?[keyPath: kind] = $0 })
    }

    private func load() async {
        do {
            let listed = try await luned.work(on: machine, project: project.id)
            listing = Listing(listed, keeping: listing)
            failed = nil
        } catch {
            failed = reason(error)
        }
    }
}

private struct Listing {
    let listed: Listed
    var issues: Set<UInt32>
    var pulls: Set<UInt32>

    init(_ listed: Listed, keeping kept: Listing?) {
        self.listed = listed
        issues = (kept?.issues ?? []).intersection(listed.issueNumbers)
        pulls = (kept?.pulls ?? []).intersection(listed.pullNumbers)
    }

    var marks: Bool { !issues.isEmpty || !pulls.isEmpty }

    var marked: Marked { Marked(issues: issues.sorted(), pulls: pulls.sorted()) }
}

extension Marked {
    fileprivate var asked: String {
        let named = issues.map { "#\($0)" } + pulls.map { "!\($0)" }
        return "Tackle \(named.joined(separator: ", "))"
    }
}

extension Listed {
    fileprivate var issueNumbers: Set<UInt32> {
        guard case .work(.open(let shown, _), _, _) = self else { return [] }
        return Set(shown.map(\.number))
    }

    fileprivate var pullNumbers: Set<UInt32> {
        guard case .work(_, let shown, _) = self else { return [] }
        return Set(shown.map(\.number))
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
