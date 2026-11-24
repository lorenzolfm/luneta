import SwiftUI

struct MachineView: View {
    @Environment(Luned.self) private var luned
    @Environment(\.dismiss) private var dismiss
    @State private var typed = ""

    var body: some View {
        Form {
            Section {
                TextField("desktop", text: $typed)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .onSubmit(reach)
                Button("Reach", action: reach)
                    .disabled(typed.trimmingCharacters(in: .whitespaces).isEmpty)
            } header: {
                Text("Machine")
            } footer: {
                Text("The tailnet name of the machine whose luned the phone watches.")
            }
            if let why = luned.reach.trouble {
                Section("luned") {
                    Text(why).foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("Machine")
        .onAppear { typed = luned.reach.host ?? "" }
    }

    private func reach() {
        guard !typed.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        luned.name(typed)
        dismiss()
    }
}
