import LunePhone
import SwiftUI
import UIKit

struct TerminalPlace: Hashable {
    let terminal: TerminalKey
    let title: String
}

private enum Seeing {
    case opening
    case shown(TerminalGrid, through: TerminalView)
    case paused(TerminalGrid)
    case closed
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
    @State private var opens: UInt64 = 0
    @State private var typing = false
    @State private var ctrl = false
    @State private var keystrokes: UInt64 = 0
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
                if case .shown(_, let view) = seeing {
                    KeyBar(
                        ctrl: $ctrl, typing: $typing,
                        press: { press($0, into: view) },
                        type: { type($0, into: view) })
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
        case .shown(let grid, let view):
            TerminalScreen(
                grid: grid, size: $size, typing: $typing, keystrokes: keystrokes,
                input: input(into: view))
        case .paused(let grid):
            TerminalScreen(
                grid: grid, size: $size, typing: $typing, keystrokes: keystrokes, input: nil)
        case .closed:
            ContentUnavailableView(
                "The terminal closed", systemImage: "terminal",
                description: Text("Its program ended, or the desktop closed it."))
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
        if case .closed = seeing { return }
        let view: TerminalView
        do {
            view = try luned.terminal(place.terminal, appearance: appearance)
        } catch {
            seeing = .ended(reason(error))
            return
        }
        failed = nil
        defer {
            view.close()
            if case .shown(let grid, let shownBy) = seeing, shownBy === view {
                seeing = .paused(grid)
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
                seeing = .shown(grid, through: view)
            case .closed:
                seeing = .closed
                return
            case .lost(let why), .refused(let why):
                seeing = .ended(why)
                return
            }
        }
    }

    private func input(into view: TerminalView) -> TerminalInput {
        TerminalInput(
            typed: { type($0, into: view) },
            deleted: { press(.backspace, into: view) },
            scroll: { back in said { try view.scroll(back: back) } })
    }

    private func type(_ text: String, into view: TerminalView) {
        let sent = said { try view.typed(text: text, ctrl: ctrl) }
        ctrl = false
        if sent {
            keystrokes &+= 1
        }
    }

    private func press(_ key: KeyName, into view: TerminalView) {
        let sent = said { try view.pressed(key: key, ctrl: ctrl) }
        ctrl = false
        if sent {
            keystrokes &+= 1
        }
    }

    @discardableResult
    private func said(_ saying: () throws -> Void) -> Bool {
        do {
            try saying()
            failed = nil
            return true
        } catch {
            failed = reason(error)
            return false
        }
    }
}

private struct KeyBar: View {
    @Binding var ctrl: Bool
    @Binding var typing: Bool
    let press: (KeyName) -> Void
    let type: (String) -> Void

    private static let symbols = ["/", "-", "|", "~"]
    private static let keyHeight: CGFloat = 36
    private static let keyWidthMin: CGFloat = 40
    private static let keyPadding: CGFloat = 10
    private static let keyboardWidthMin: CGFloat = 48
    private static let dividerHeight = keyHeight * 0.6

    var body: some View {
        HStack(spacing: 0) {
            ScrollView(.horizontal) {
                HStack(spacing: 0) {
                    key("esc") { press(.escape) }
                    key("tab") { press(.tab) }
                    Button {
                        ctrl.toggle()
                    } label: {
                        keyFace("ctrl")
                            .foregroundStyle(ctrl ? Color(.systemBackground) : Color.accentColor)
                            .background(
                                ctrl ? Color.accentColor : .clear,
                                in: RoundedRectangle(cornerRadius: 8))
                    }
                    .accessibilityAddTraits(ctrl ? .isSelected : [])
                    arrow("Left", "arrow.left", .left)
                    arrow("Up", "arrow.up", .up)
                    arrow("Down", "arrow.down", .down)
                    arrow("Right", "arrow.right", .right)
                    ForEach(KeyBar.symbols, id: \.self) { symbol in
                        key(symbol) { type(symbol) }
                    }
                }
                .padding(.horizontal, 4)
            }
            .scrollIndicators(.hidden)
            Divider()
                .frame(height: KeyBar.dividerHeight)
            Button(
                typing ? "Hide the keyboard" : "Show the keyboard",
                systemImage: typing ? "keyboard.chevron.compact.down" : "keyboard"
            ) {
                typing.toggle()
            }
            .labelStyle(.iconOnly)
            .frame(minWidth: KeyBar.keyboardWidthMin, minHeight: KeyBar.keyHeight)
        }
        .buttonStyle(.borderless)
        .font(.system(.body, design: .monospaced))
        .dynamicTypeSize(...DynamicTypeSize.xLarge)
        .padding(.vertical, 4)
        .background(.bar)
    }

    private func key(_ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { keyFace(label) }
    }

    private func arrow(_ label: String, _ systemImage: String, _ key: KeyName) -> some View {
        Button(label, systemImage: systemImage) { press(key) }
            .labelStyle(.iconOnly)
            .frame(minWidth: KeyBar.keyWidthMin, minHeight: KeyBar.keyHeight)
    }

    private func keyFace(_ label: String) -> some View {
        Text(label)
            .fixedSize()
            .padding(.horizontal, KeyBar.keyPadding)
            .frame(minWidth: KeyBar.keyWidthMin, minHeight: KeyBar.keyHeight)
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
