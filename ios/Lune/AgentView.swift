import LunePhone
import SwiftUI

struct AgentView: View {
    let seat: Seat
    @Environment(Luned.self) private var luned
    @State private var waiting: Waiting?
    @State private var unread: String?
    @State private var unanswered: String?
    @State private var loads: UInt64 = 0
    @State private var words = ""
    @State private var answering = false
    @State private var asking: AskPlace?

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
            if let unanswered {
                Section {
                    Text(unanswered).foregroundStyle(.red)
                }
            }
            if let unread {
                Section {
                    Text(unread).foregroundStyle(.red)
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
                    answers(to: waiting)
                    Button("Escape", role: .destructive) { answer(.escape, on: waiting.at) }
                }
                .disabled(answering)
            } else if unread == nil {
                ProgressView()
            }
        }
        .navigationTitle(agent?.name ?? "Agent")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let placed = agent?.placed,
                let worktree = luned.worktree(placed.worktree, on: seat.machine)
            {
                Button("Ask here", systemImage: "bubble.left") {
                    asking = AskPlace(
                        machine: seat.machine, cwd: worktree.folder, title: worktree.name)
                }
            }
        }
        .sheet(item: $asking) { place in
            NavigationStack { AskView(place: place, opening: nil) }
        }
        .refreshable { await load() }
        .task(id: agent) { await load() }
    }

    @ViewBuilder
    private func answers(to waiting: Waiting) -> some View {
        switch waiting.asks {
        case .menu(let choices, let chosen):
            ForEach(Array(choices.enumerated()), id: \.offset) { index, label in
                Button {
                    answer(.choice(index: UInt32(index), label: label), on: waiting.at)
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
            Button("Send") { answer(.text(text: words), on: waiting.at) }
                .disabled(words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        case .unread(let why):
            Text(why).foregroundStyle(.secondary)
        }
    }

    private func load() async {
        loads += 1
        let load = loads
        let read: Result<Waiting, Error>
        do {
            read = .success(try await luned.waiting(at: seat))
        } catch {
            read = .failure(error)
        }
        guard load == loads else { return }
        switch read {
        case .success(let read):
            waiting = read
            unread = nil
        case .failure(let error):
            unread = reason(error)
        }
    }

    private func answer(_ answer: Answer, on screen: Screen) {
        precondition(!answering)
        answering = true
        Task {
            do {
                try await luned.answer(answer, on: screen)
                words = ""
                unanswered = nil
            } catch {
                unanswered = reason(error)
            }
            await load()
            answering = false
        }
    }
}
