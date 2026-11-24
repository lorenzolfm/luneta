import LunePhone
import SwiftUI
import UIKit

struct TerminalFont {
    static let readable: CGFloat = 11
    static let largest: CGFloat = 28
    static let smallest: CGFloat = 2

    private static let measured: CGFloat = 100
    private static let measuring = UIFont.monospacedSystemFont(ofSize: measured, weight: .regular)
    static let advance = ("M" as NSString).size(withAttributes: [.font: measuring]).width / measured
    static let leading = measuring.lineHeight / measured

    static func cell(_ size: CGFloat) -> CGSize {
        CGSize(width: size * advance, height: size * leading)
    }

    static func fitting(_ columns: UInt16, in width: CGFloat) -> CGFloat {
        let fit = width / (CGFloat(max(columns, 1)) * advance)
        return min(max(fit, smallest), largest)
    }
}

struct TerminalInput {
    let typed: (String) -> Void
    let deleted: () -> Void
    let scroll: (Int32) -> Void
}

struct Pull: Equatable {
    let pastTop: CGFloat
    let pastBottom: CGFloat
}

struct TerminalScreen: View {
    let grid: TerminalGrid
    @Binding var size: CGFloat
    @Binding var typing: Bool
    let input: TerminalInput?
    @State private var pinched: CGFloat = 1
    @State private var pulled = false

    private static let cursorAt = "cursor"
    private static let pullFar: CGFloat = 48

    var body: some View {
        GeometryReader { outer in
            let shown = min(max(size * pinched, TerminalFont.smallest), TerminalFont.largest)
            let cell = TerminalFont.cell(shown)
            let fit = TerminalFont.fitting(grid.width, in: outer.size.width)
            ScrollViewReader { reader in
                ScrollView([.horizontal, .vertical]) {
                    ZStack(alignment: .topLeading) {
                        GridCanvas(grid: grid, size: shown, cell: cell)
                            .frame(
                                width: cell.width * CGFloat(grid.width),
                                height: cell.height * CGFloat(grid.rows.count))
                        if let cursor = grid.cursor {
                            Color.clear
                                .frame(width: cell.width, height: cell.height)
                                .padding(.leading, CGFloat(cursor.column) * cell.width)
                                .padding(.top, CGFloat(cursor.row) * cell.height)
                                .id(TerminalScreen.cursorAt)
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture(count: 2) {
                        size = abs(size - fit) < 0.5 ? TerminalFont.readable : fit
                    }
                    .onTapGesture { typing = input != nil }
                }
                .scrollBounceBehavior(.always, axes: .vertical)
                .background(Color(grid.background))
                .simultaneousGesture(
                    MagnifyGesture()
                        .onChanged { pinched = $0.magnification }
                        .onEnded { ended in
                            size = min(
                                max(size * ended.magnification, TerminalFont.smallest),
                                TerminalFont.largest)
                            pinched = 1
                        }
                )
                .onScrollGeometryChange(for: Pull.self) { geometry in
                    let top = -(geometry.contentOffset.y + geometry.contentInsets.top)
                    let seen = geometry.contentOffset.y + geometry.containerSize.height
                    let bottom =
                        seen - geometry.contentInsets.bottom - geometry.contentSize.height
                    return Pull(pastTop: top, pastBottom: bottom)
                } action: { _, pull in
                    if let input {
                        pulledTo(pull, scroll: input.scroll)
                    }
                }
                .onChange(of: grid.cursor) {
                    if typing {
                        reader.scrollTo(TerminalScreen.cursorAt, anchor: .center)
                    }
                }
                .onAppear { reader.scrollTo(TerminalScreen.cursorAt, anchor: .center) }
            }
            .overlay(alignment: .top) { scrolledBack }
            .background {
                if let input {
                    KeyInput(typing: $typing, typed: input.typed, deleted: input.deleted)
                        .frame(width: 0, height: 0)
                }
            }
        }
    }

    @ViewBuilder
    private var scrolledBack: some View {
        if grid.scrolledBack > 0 {
            HStack {
                Text("\(grid.scrolledBack) of \(grid.scrollback) lines back")
                    .font(.caption)
                if let input {
                    Button("Now") { input.scroll(-Int32(clamping: grid.scrolledBack)) }
                        .font(.caption.bold())
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(.thinMaterial, in: Capsule())
            .padding(.top, 8)
        }
    }

    private func pulledTo(_ pull: Pull, scroll: (Int32) -> Void) {
        let far = TerminalScreen.pullFar
        if pull.pastTop < far / 2 && pull.pastBottom < far / 2 {
            pulled = false
            return
        }
        guard !pulled else { return }
        let page = Int32(clamping: max(grid.rows.count / 2, 1))
        if pull.pastTop >= far {
            pulled = true
            scroll(page)
        } else if pull.pastBottom >= far && grid.scrolledBack > 0 {
            pulled = true
            scroll(-page)
        }
    }
}

struct GridCanvas: View {
    let grid: TerminalGrid
    let size: CGFloat
    let cell: CGSize

    var body: some View {
        let font = Font.system(size: size, design: .monospaced)
        Canvas { context, _ in
            for (index, row) in grid.rows.enumerated() {
                let top = CGFloat(index) * cell.height
                for run in row.runs {
                    let place = CGRect(
                        x: CGFloat(run.column) * cell.width, y: top,
                        width: CGFloat(run.width) * cell.width, height: cell.height)
                    if let background = run.background {
                        context.fill(Path(place), with: .color(Color(background)))
                    }
                    var text = Text(run.text).font(font).foregroundStyle(Color(run.foreground))
                    if run.bold { text = text.bold() }
                    if run.italic { text = text.italic() }
                    if run.underlined { text = text.underline() }
                    if run.struck { text = text.strikethrough() }
                    if run.width == 2 && run.text.count == 1 {
                        context.draw(
                            text, at: CGPoint(x: place.midX, y: place.midY), anchor: .center)
                    } else {
                        context.draw(text, at: place.origin, anchor: .topLeading)
                    }
                }
            }
            if let cursor = grid.cursor {
                context.stroke(
                    cursorPath(cursor), with: .color(Color(grid.foreground).opacity(0.8)),
                    lineWidth: 1)
                if cursor.shape != .blockHollow {
                    context.fill(
                        cursorPath(cursor), with: .color(Color(grid.foreground).opacity(0.4)))
                }
            }
        }
    }

    private func cursorPath(_ cursor: TerminalCursor) -> Path {
        let left = CGFloat(cursor.column) * cell.width
        let top = CGFloat(cursor.row) * cell.height
        let line = max(cell.height / 10, 1)
        switch cursor.shape {
        case .block, .blockHollow:
            return Path(CGRect(x: left, y: top, width: cell.width, height: cell.height))
        case .bar:
            return Path(CGRect(x: left, y: top, width: line, height: cell.height))
        case .underline:
            return Path(
                CGRect(x: left, y: top + cell.height - line, width: cell.width, height: line))
        }
    }
}

extension Color {
    init(_ rgb: Rgb) {
        self.init(
            red: Double(rgb.red) / 255, green: Double(rgb.green) / 255,
            blue: Double(rgb.blue) / 255)
    }
}
