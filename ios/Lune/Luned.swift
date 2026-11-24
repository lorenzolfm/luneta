import Foundation
import LunePhone
import Observation

struct Seat: Hashable {
    let machine: String?
    let terminal: TerminalKey
}

struct ProjectSeat: Hashable {
    let machine: String?
    let project: UInt64
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

    func held(on machine: String?) -> Held? {
        guard let model else { return nil }
        guard let machine else { return model.here }
        return model.far.first { $0.on == machine }?.held
    }

    func project(at seat: ProjectSeat) -> Project? {
        guard case .kept(let projects)? = held(on: seat.machine) else { return nil }
        return projects.first { $0.id == seat.project }
    }

    func worktree(_ id: UInt64, on machine: String?) -> Worktree? {
        guard case .kept(let projects)? = held(on: machine) else { return nil }
        return projects.lazy.flatMap(\.holds.worktrees).first { $0.id == id }
    }

    func ask(on machine: String?, in cwd: String?, words: String, resumed: String?) throws
        -> Asking
    {
        let marked = Marked(issues: [], pulls: [])
        return try reached().ask(
            on: machine, cwd: cwd, words: words, resumed: resumed, marked: marked)
    }

    func tackle(on machine: String?, in dir: String, marked: Marked) throws -> Asking {
        try reached().tackle(on: machine, dir: dir, marked: marked)
    }

    func work(on machine: String?, project: UInt64) async throws -> Listed {
        try await reached().work(on: machine, project: project)
    }

    func newAgent(on machine: String?, project: UInt64, branch: String, words: String)
        async throws -> Landed
    {
        try await reached().newAgent(on: machine, project: project, branch: branch, words: words)
    }

    func close(_ view: TerminalKey, on machine: String?) async throws {
        try await reached().closeView(on: machine, terminal: view)
    }

    func close(_ worktree: Worktree, on machine: String?) async throws {
        try await reached().closeWorktree(
            on: machine, worktree: worktree.id, seen: worktree.views.map(\.id))
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
