import LunePhone
import SwiftUI
import UIKit

struct TerminalPlace: Hashable {
    let terminal: TerminalKey
    let title: String
}

private enum Seeing {
    case opening
    case shown(TerminalGrid)
    case ended(String)
}

private struct Viewing: Equatable {
    let opens: UInt64
    let active: Bool
    let appearance: Appearance
}

struct TerminalPage: View {
    let place: TerminalPlace
    @Environment(Luned.self) private var luned
    @Environment(\.colorScheme) private var scheme
    @Environment(\.scenePhase) private var phase
    @State private var seeing = Seeing.opening
    @State private var viewing: TerminalView?
    @State private var opens: UInt64 = 0
    @State private var typing = false
    @State private var ctrl = false
    @State private var size = TerminalFont.readable
    @State private var failed: String?

    var body: some View {
        content
            .navigationTitle(place.title)
            .navigationBarTitleDisplayMode(.inline)
            .safeAreaInset(edge: .top) {
                if let failed {
                    Label(failed, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.red)
                        .padding(.horizontal)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.bar)
                }
            }
            .safeAreaInset(edge: .bottom) {
                if case .shown = seeing {
                    KeyBar(ctrl: $ctrl, typing: $typing, press: press)
                }
            }
            .task(id: Viewing(opens: opens, active: phase == .active, appearance: appearance)) {
                await show()
            }
    }

    private var appearance: Appearance {
        scheme == .dark ? .dark : .light
    }

    @ViewBuilder
    private var content: some View {
        switch seeing {
        case .opening:
            ProgressView("Opening the terminal")
        case .shown(let grid):
            TerminalScreen(
                grid: grid, size: $size, typing: $typing, typed: type,
                deleted: { press(.backspace) }, scroll: scroll)
        case .ended(let why):
            ContentUnavailableView {
                Label("The terminal is not shown", systemImage: "terminal")
            } description: {
                Text(why)
            } actions: {
                Button("Retry") { opens += 1 }
            }
        }
    }

    private func show() async {
        guard phase == .active else { return }
        let view: TerminalView
        do {
            view = try luned.terminal(place.terminal, appearance: appearance)
        } catch {
            seeing = .ended(reason(error))
            return
        }
        viewing = view
        failed = nil
        defer {
            view.close()
            if viewing === view {
                viewing = nil
            }
        }
        while !Task.isCancelled {
            let framed = await withTaskCancellationHandler {
                await view.next()
            } onCancel: {
                view.close()
            }
            guard !Task.isCancelled else { return }
            switch framed {
            case .shown(let grid):
                seeing = .shown(grid)
            case .lost(let why), .refused(let why):
                seeing = .ended(why)
                return
            }
        }
    }

    private func type(_ text: String) {
        said { try $0.typed(text: text, ctrl: ctrl) }
        ctrl = false
    }

    private func press(_ key: KeyName) {
        said { try $0.pressed(key: key, ctrl: ctrl) }
        ctrl = false
    }

    private func scroll(_ back: Int32) {
        said { try $0.scroll(back: back) }
    }

    private func said(_ saying: (TerminalView) throws -> Void) {
        guard let viewing else {
            failed = "the terminal is opening again, so that went nowhere"
            return
        }
        do {
            try saying(viewing)
            failed = nil
        } catch {
            failed = reason(error)
        }
    }
}

private struct KeyBar: View {
    @Binding var ctrl: Bool
    @Binding var typing: Bool
    let press: (KeyName) -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button("esc") { press(.escape) }
            Button("tab") { press(.tab) }
            Toggle("ctrl", isOn: $ctrl).toggleStyle(.button)
            Spacer()
            Button("Left", systemImage: "arrow.left") { press(.left) }
            Button("Up", systemImage: "arrow.up") { press(.up) }
            Button("Down", systemImage: "arrow.down") { press(.down) }
            Button("Right", systemImage: "arrow.right") { press(.right) }
            Spacer()
            Button(
                typing ? "Hide the keyboard" : "Show the keyboard",
                systemImage: typing ? "keyboard.chevron.compact.down" : "keyboard"
            ) {
                typing.toggle()
            }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.bordered)
        .font(.system(.body, design: .monospaced))
        .padding(.horizontal)
        .padding(.vertical, 6)
        .background(.bar)
    }
}

struct KeyInput: UIViewRepresentable {
    @Binding var typing: Bool
    let typed: (String) -> Void
    let deleted: () -> Void

    func makeUIView(context: Context) -> KeyField {
        KeyField()
    }

    func updateUIView(_ field: KeyField, context: Context) {
        field.typed = typed
        field.deleted = deleted
        field.left = { typing = false }
        if typing && !field.isFirstResponder {
            Task { field.becomeFirstResponder() }
        } else if !typing && field.isFirstResponder {
            Task { field.resignFirstResponder() }
        }
    }
}

final class KeyField: UIView, UIKeyInput {
    var typed: (String) -> Void = { _ in }
    var deleted: () -> Void = {}
    var left: () -> Void = {}
    var autocorrectionType = UITextAutocorrectionType.no
    var autocapitalizationType = UITextAutocapitalizationType.none
    var spellCheckingType = UITextSpellCheckingType.no
    var smartQuotesType = UITextSmartQuotesType.no
    var smartDashesType = UITextSmartDashesType.no
    var smartInsertDeleteType = UITextSmartInsertDeleteType.no
    var keyboardType = UIKeyboardType.asciiCapable

    override var canBecomeFirstResponder: Bool { true }

    var hasText: Bool { true }

    func insertText(_ text: String) {
        typed(text)
    }

    func deleteBackward() {
        deleted()
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned {
            left()
        }
        return resigned
    }
}
