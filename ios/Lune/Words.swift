import LunePhone

extension Agent {
    var seat: Seat? {
        terminal.map { Seat(machine: on, terminal: $0) }
    }

    var waits: Bool {
        if case .waiting = status { true } else { false }
    }

    var place: String? {
        let placed = placed.map { "\($0.projectName) › \($0.worktreeName)" }
        switch (on, placed) {
        case (let machine?, let placed?): return "\(machine) · \(placed)"
        case (let machine?, nil): return machine
        case (nil, let placed): return placed
        }
    }
}

extension Status {
    var words: String {
        switch self {
        case .busy: "busy"
        case .idle: "idle"
        case .waiting(let why): why.words
        case .shell: "at its shell"
        case .other(let said): said
        case .untold: "says nothing of itself"
        }
    }
}

extension Why {
    var words: String {
        switch self {
        case .permission: "asks for permission"
        case .input: "waits for your words"
        case .dialog: "shows a dialog"
        case .other(let said): said
        case .untold: "waits"
        }
    }
}
