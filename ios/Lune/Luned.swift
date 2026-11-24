import Foundation
import LunePhone
import Observation

enum Heard: Equatable {
    case nothing
    case model(Model)
    case lost(String)
}

@Observable
final class Luned {
    private static let hostKey = "host"

    private(set) var host: String
    private(set) var heard = Heard.nothing
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
        heard = .nothing
    }

    fileprivate func changed(_ model: Model) {
        heard = .model(model)
    }

    fileprivate func lost(_ why: String) {
        heard = .lost(why)
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
