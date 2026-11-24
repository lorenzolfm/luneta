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

extension Holds {
    var worktrees: [Worktree] {
        switch self {
        case .folder(let worktree): [worktree]
        case .clone(let worktrees, _): worktrees
        case .untold: []
        }
    }

    var unlisted: String? {
        switch self {
        case .clone(_, .unlisted(let why)): why
        case .untold: "luned says nothing of this project's worktrees"
        case .folder, .clone(_, .listed), .clone(_, .untold): nil
        }
    }
}

extension Seen {
    var words: String? {
        switch self {
        case .missing: "missing"
        case .unread: "unread"
        case .noBranch: "no branch"
        case .working: nil
        case .pullRequestOpen: "pull request open"
        case .checksFailing: "checks failing"
        case .merged: "merged"
        case .untold: nil
        }
    }
}

extension LuneView {
    var title: String {
        name ?? topic ?? command ?? "shell"
    }
}

extension Landed {
    var words: String {
        switch self {
        case .started: "The agent starts in a new worktree."
        case .found: "The branch had a worktree already, and the agent opens there."
        case .shell: "luned made the worktree with a shell in it."
        }
    }
}
