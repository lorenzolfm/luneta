import LunePhone
import SwiftUI

struct AskPlace: Identifiable, Hashable {
    let machine: String?
    let cwd: String?
    let title: String

    var id: Self { self }
}

enum Opening {
    case tackle(marked: Marked, asked: String)
}

nonisolated private final class Listener {
    var listening: Task<Void, Never>?

    deinit {
        listening?.cancel()
    }
}

private struct Turn: Identifiable {
    let id = UUID()
    let words: String
    var steps: [Step] = []
    var failed: String?
}

struct AskView: View {
    let place: AskPlace
    let opening: Opening?
    @Environment(Luned.self) private var luned
    @Environment(\.dismiss) private var dismiss
    @State private var turns: [Turn] = []
    @State private var words = ""
    @State private var conversation: Conversation?
    @State private var asking: Asking?
    @State private var listener = Listener()

    var body: some View {
        List {
            if turns.isEmpty {
                Text(
                    place.cwd.map { "Claude answers in \($0)" }
                        ?? "Claude answers in your home folder"
                )
                .foregroundStyle(.secondary)
            }
            ForEach(turns) { turn in
                Section {
                    ForEach(Array(turn.steps.enumerated()), id: \.offset) { _, step in
                        StepRow(step: step)
                    }
                    if let failed = turn.failed {
                        Text(failed).foregroundStyle(.red)
                    }
                } header: {
                    Text(turn.words).textCase(nil).font(.body.bold()).foregroundStyle(.primary)
                }
            }
            if asking != nil {
                ProgressView()
            }
        }
        .defaultScrollAnchor(.bottom)
        .navigationTitle(place.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if place.cwd != nil || opening != nil {
                Button("Done") { dismiss() }
            }
        }
        .safeAreaInset(edge: .bottom) {
            HStack(alignment: .bottom) {
                TextField(
                    conversation?.resumes() == true ? "Answer" : "Ask", text: $words,
                    axis: .vertical
                )
                .lineLimit(1...6)
                .textFieldStyle(.roundedBorder)
                Button("Send", systemImage: "arrow.up.circle.fill", action: send)
                    .labelStyle(.iconOnly)
                    .font(.title2)
                    .disabled(
                        asking != nil
                            || words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding()
            .background(.bar)
        }
        .task {
            guard case .tackle(let marked, let asked)? = opening, turns.isEmpty else {
                return
            }
            start(asked) { try $0.tackle(marked: marked) }
        }
    }

    private func send() {
        let asked = words.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !asked.isEmpty else { return }
        words = ""
        start(asked) { try $0.ask(words: asked) }
    }

    private func start(_ asked: String, _ ask: (Conversation) throws -> Asking) {
        turns.append(Turn(words: asked))
        let index = turns.count - 1
        let talk: Asking
        do {
            let conversation =
                try self.conversation ?? luned.converse(on: place.machine, in: place.cwd)
            self.conversation = conversation
            talk = try ask(conversation)
        } catch {
            turns[index].failed = reason(error)
            return
        }
        asking = talk
        listener.listening = Task {
            do {
                while !Task.isCancelled, let step = try await talk.next() {
                    turns[index].steps.append(step)
                }
            } catch {
                turns[index].failed = reason(error)
            }
            precondition(asking === talk)
            asking = nil
        }
    }
}

private struct StepRow: View {
    let step: Step

    var body: some View {
        switch step {
        case .said(let text):
            Text(LocalizedStringKey(text)).textSelection(.enabled)
        case .ran(_, let command):
            Text("$ \(command)")
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
        case .denied:
            Text("A command was denied").font(.caption).foregroundStyle(.orange)
        }
    }
}
