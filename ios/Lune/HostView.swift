import LunePhone
import SwiftUI

struct HostView: View {
    @Environment(Luned.self) private var luned
    @State private var typed = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Machine") {
                    TextField("desktop", text: $typed)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit { luned.reach(typed) }
                    Button("Reach") { luned.reach(typed) }
                        .disabled(typed.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                Section("luned") {
                    if let refused = luned.refused {
                        Text(refused).foregroundStyle(.red)
                    }
                    switch luned.heard {
                    case .nothing:
                        Text(luned.host.isEmpty ? "No machine named" : "Reaching \(luned.host)…")
                            .foregroundStyle(.secondary)
                    case .lost(let why):
                        Text(why).foregroundStyle(.red)
                    case .model(let model):
                        LabeledContent("Agents", value: "\(model.inbox.count)")
                        LabeledContent("Far machines", value: "\(model.far.count)")
                    }
                }
            }
            .navigationTitle("Lune")
        }
        .onAppear { typed = luned.host }
    }
}
