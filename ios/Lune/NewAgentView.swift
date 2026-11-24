import LunePhone
import SwiftUI

struct NewAgentView: View {
    let machine: String?
    let project: Project
    @Environment(Luned.self) private var luned
    @Environment(\.dismiss) private var dismiss
    @State private var branch = ""
    @State private var words = ""
    @State private var starting = false
    @State private var landed: Landed?
    @State private var failed: String?

    var body: some View {
        Form {
            Section("Branch") {
                TextField("fix-the-thing", text: $branch)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
            Section("What the agent does") {
                TextField("Words for Claude", text: $words, axis: .vertical)
                    .lineLimit(3...10)
            }
            if let landed {
                Section {
                    Text(landed.words)
                }
            }
            if let failed {
                Section {
                    Text(failed).foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("New agent in \(project.name)")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button(landed == nil ? "Cancel" : "Done") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Start", action: start).disabled(!ready)
            }
        }
        .disabled(starting)
    }

    private var ready: Bool {
        landed == nil && !branch.trimmingCharacters(in: .whitespaces).isEmpty
            && !words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func start() {
        starting = true
        Task {
            do {
                landed = try await luned.newAgent(
                    on: machine, project: project.id,
                    branch: branch.trimmingCharacters(in: .whitespaces), words: words)
                failed = nil
            } catch {
                failed = said(error)
            }
            starting = false
        }
    }
}
