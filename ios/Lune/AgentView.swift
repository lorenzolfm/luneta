import LunePhone
import SwiftUI

struct AgentView: View {
    let seat: Seat
    @Environment(Luned.self) private var luned
    @State private var waiting: Waiting?
    @State private var failed: String?
    @State private var words = ""
    @State private var answering = false

    var body: some View {
        let agent = luned.agent(at: seat)
        List {
            if let agent {
                Section {
                    if let place = agent.place {
                        Text(place).foregroundStyle(.secondary)
                    }
                    Text(agent.status.words)
                        .foregroundStyle(agent.waits ? .orange : .secondary)
                }
            } else {
                Section {
                    Text("luned no longer shows this agent").foregroundStyle(.secondary)
                }
            }
            if let failed {
                Section {
                    Text(failed).foregroundStyle(.red)
                }
            }
            if let waiting {
                Section("Screen") {
                    ScrollView(.horizontal) {
                        Text(waiting.screen)
                            .font(.system(.caption2, design: .monospaced))
                            .textSelection(.enabled)
                            .fixedSize()
                    }
                    .defaultScrollAnchor(.bottomLeading)
                }
                if let said = waiting.said {
                    Section("Last message") {
                        Text(said).textSelection(.enabled)
                    }
                }
                Section("Answer") {
                    answers(to: waiting.asks)
                    Button("Escape", role: .destructive) { answer(.escape) }
                }
                .disabled(answering)
            } else if failed == nil {
                ProgressView()
            }
        }
        .navigationTitle(agent?.name ?? "Agent")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await load() }
        .task(id: agent) { await load() }
    }

    @ViewBuilder
    private func answers(to asks: Asks) -> some View {
        switch asks {
        case .menu(let choices, let chosen):
            ForEach(Array(choices.enumerated()), id: \.offset) { index, label in
                Button {
                    answer(.choice(index: UInt32(index), label: label))
                } label: {
                    HStack {
                        Text("\(index + 1). \(label)")
                        Spacer()
                        if index == chosen {
                            Image(systemName: "arrowtriangle.left.fill")
                                .accessibilityLabel("Chosen on the screen")
                        }
                    }
                }
            }
        case .words:
            TextField("Your words", text: $words, axis: .vertical)
                .lineLimit(1...8)
            Button("Send") { answer(.text(text: words)) }
                .disabled(words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        case .unread(let why):
            Text(why).foregroundStyle(.secondary)
        }
    }

    private func load() async {
        do {
            waiting = try await luned.waiting(at: seat)
            failed = nil
        } catch {
            failed = said(error)
        }
    }

    private func answer(_ answer: Answer) {
        guard let shown = waiting?.shown else { return }
        answering = true
        Task {
            do {
                try await luned.answer(at: seat, shown: shown, with: answer)
                words = ""
                failed = nil
            } catch {
                failed = said(error)
            }
            await load()
            answering = false
        }
    }
}
