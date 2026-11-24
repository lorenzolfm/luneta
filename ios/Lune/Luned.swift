import Foundation
import LunePhone
import Observation

struct Seat: Hashable {
    let machine: String?
    let terminal: TerminalKey
}

@Observable
final class Luned {
    private static let hostKey = "host"

    private(set) var host: String
    private(set) var model: Model?
    private(set) var lost: String?
    private(set) var refused: String?
    private var core: Core?

    init() {
        host = UserDefaults.standard.string(forKey: Luned.hostKey) ?? ""
    }

    func reach(_ named: String) {
        host = named.trimmingCharacters(in: .whitespaces)
        UserDefaults.standard.set(host, forKey: Luned.hostKey)
        close()
        core = nil
        model = nil
        open()
    }

    func open() {
        guard !host.isEmpty else { return }
        do {
            let core = try self.core ?? Core(host: host)
            self.core = core
            refused = nil
            core.watch(observer: Watcher(luned: self))
        } catch {
            refused = said(error)
        }
    }

    func close() {
        core?.stop()
        lost = nil
    }

    func agent(at seat: Seat) -> Agent? {
        model?.inbox.first { $0.seat == seat }
    }

    func waiting(at seat: Seat) async throws -> Waiting {
        try await reached().waiting(on: seat.machine, terminal: seat.terminal)
    }

    func answer(at seat: Seat, shown: UInt64, with answer: Answer) async throws {
        try await reached().answer(
            on: seat.machine, terminal: seat.terminal, shown: shown, answer: answer)
    }

    private func reached() throws -> Core {
        guard let core else { throw Unanswered.Refused(why: "no machine is named") }
        return core
    }

    fileprivate func changed(_ model: Model) {
        self.model = model
        lost = nil
    }

    fileprivate func lost(_ why: String) {
        lost = why
    }
}

func said(_ error: Error) -> String {
    switch error {
    case Unanswered.Refused(let why), Unanswered.InDoubt(let why):
        why
    default:
        error.localizedDescription
    }
}

nonisolated private final class Watcher: Observer {
    private let luned: Luned

    init(luned: Luned) {
        self.luned = luned
    }

    func changed(model: Model) {
        Task { @MainActor in luned.changed(model) }
    }

    func lost(why: String) {
        Task { @MainActor in luned.lost(why) }
    }
}
