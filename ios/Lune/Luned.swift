import Foundation
import LunePhone
import Observation

struct ProjectSeat: Hashable {
    let machine: String?
    let project: UInt64
}

enum Heard {
    case nothing
    case model(Model, lost: String?)
    case lost(String)
}

enum Reach {
    case unnamed
    case refused(host: String, why: String)
    case watching(host: String, core: Core, heard: Heard)

    var host: String? {
        switch self {
        case .unnamed: nil
        case .refused(let host, _), .watching(let host, _, _): host
        }
    }

    var model: Model? {
        guard case .watching(_, _, .model(let model, _)) = self else { return nil }
        return model
    }

    var trouble: String? {
        switch self {
        case .unnamed, .watching(_, _, .nothing), .watching(_, _, .model(_, .none)): nil
        case .refused(_, let why), .watching(_, _, .lost(let why)),
            .watching(_, _, .model(_, .some(let why))):
            why
        }
    }
}

@Observable
final class Luned {
    private static let hostKey = "host"

    private(set) var reach: Reach
    private var watches: UInt64 = 0

    init() {
        reach = UserDefaults.standard.string(forKey: Luned.hostKey).map(Luned.reaching) ?? .unnamed
    }

    var model: Model? { reach.model }

    func name(_ named: String) {
        unwatch()
        reach = Luned.reaching(named)
        if case .watching(let host, _, _) = reach {
            UserDefaults.standard.set(host, forKey: Luned.hostKey)
        }
        watch()
    }

    func watch() {
        if case .refused(let host, _) = reach {
            reach = Luned.reaching(host)
        }
        guard case .watching(_, let core, _) = reach else { return }
        watches += 1
        core.watch(observer: Watcher(luned: self, watch: watches))
    }

    func unwatch() {
        guard case .watching(let host, let core, let heard) = reach else { return }
        core.stop()
        watches += 1
        reach = .watching(host: host, core: core, heard: heard.unwatched)
    }

    func agent(at terminal: TerminalKey) -> Agent? {
        model?.inbox.first { $0.terminal == terminal }
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

    func converse(on machine: String?, in cwd: String?) throws -> Conversation {
        try reached().converse(on: machine, cwd: cwd)
    }

    func work(on machine: String?, project: UInt64) async throws -> Listed {
        try await reached().work(on: machine, project: project)
    }

    func newAgent(on machine: String?, project: UInt64, branch: String, words: String)
        async throws -> Landed
    {
        try await reached().newAgent(on: machine, project: project, branch: branch, words: words)
    }

    func close(_ view: TerminalKey) async throws {
        try await reached().closeView(terminal: view)
    }

    func close(_ worktree: Worktree, on machine: String?) async throws {
        try await reached().closeWorktree(
            on: machine, worktree: worktree.id, seen: worktree.views.map(\.id),
            seenBrowsers: worktree.browsers)
    }

    func startFlow(
        on machine: String?, worktree: UInt64, interactive: Bool, prompt: String? = nil
    ) async throws {
        try await reached().startFlow(
            on: machine, worktree: worktree, interactive: interactive, prompt: prompt)
    }

    func controlFlow(on machine: String?, worktree: UInt64, control: FlowControl) async throws {
        try await reached().controlFlow(on: machine, worktree: worktree, control: control)
    }

    func waiting(at terminal: TerminalKey) async throws -> Waiting {
        try await reached().waiting(terminal: terminal)
    }

    func answer(_ answer: Answer, on screen: Screen) async throws {
        try await reached().answer(screen: screen, answer: answer)
    }

    func terminal(_ terminal: TerminalKey, appearance: Appearance) throws -> TerminalView {
        try reached().terminal(terminal: terminal, appearance: appearance)
    }

    private func reached() throws -> Core {
        switch reach {
        case .unnamed: throw Unanswered.Refused(why: "no machine is named")
        case .refused(_, let why): throw Unanswered.Refused(why: why)
        case .watching(_, let core, _): return core
        }
    }

    private static func reaching(_ named: String) -> Reach {
        do {
            let host = try tailnetName(named: named)
            return .watching(host: host, core: try Core(host: host), heard: .nothing)
        } catch {
            return .refused(host: named, why: reason(error))
        }
    }

    fileprivate func changed(_ model: Model, in watch: UInt64) {
        hear(in: watch) { _ in .model(model, lost: nil) }
    }

    fileprivate func lost(_ why: String, in watch: UInt64) {
        hear(in: watch) { heard in
            switch heard {
            case .model(let model, _): .model(model, lost: why)
            case .nothing, .lost: .lost(why)
            }
        }
    }

    private func hear(in watch: UInt64, _ hearing: (Heard) -> Heard) {
        precondition(watch <= watches)
        guard watch == watches, case .watching(let host, let core, let heard) = reach else {
            return
        }
        reach = .watching(host: host, core: core, heard: hearing(heard))
    }
}

extension Heard {
    fileprivate var unwatched: Heard {
        switch self {
        case .nothing, .lost: .nothing
        case .model(let model, _): .model(model, lost: nil)
        }
    }
}

func reason(_ error: Error) -> String {
    switch error {
    case Unanswered.Refused(let why), Unanswered.InDoubt(let why):
        why
    default:
        error.localizedDescription
    }
}

nonisolated private final class Watcher: Observer {
    private let luned: Luned
    private let watch: UInt64

    init(luned: Luned, watch: UInt64) {
        self.luned = luned
        self.watch = watch
    }

    func changed(model: Model) {
        Task { @MainActor in luned.changed(model, in: watch) }
    }

    func lost(why: String) {
        Task { @MainActor in luned.lost(why, in: watch) }
    }
}
