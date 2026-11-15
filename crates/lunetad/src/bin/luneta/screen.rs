use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Color as Colour, NamedColor};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

fn within(area: Rect, line: i32, column: usize) -> Option<Position> {
    let row = u16::try_from(line).ok().filter(|row| *row < area.height)?;
    let column = u16::try_from(column).ok().filter(|column| *column < area.width)?;
    Some(Position::new(area.x + column, area.y + row))
}

pub fn show<T: EventListener>(screen: &Term<T>, buffer: &mut Buffer, area: Rect, top: usize) {
    let content = screen.renderable_content();
    assert_eq!(content.display_offset, 0);
    let top = i32::try_from(top).unwrap_or(i32::MAX);
    for indexed in content.display_iter {
        let line = indexed.point.line.0.saturating_sub(top);
        let Some(position) = within(area, line, indexed.point.column.0) else {
            continue;
        };
        let Some(drawn) = buffer.cell_mut(position) else {
            continue;
        };
        let cell = indexed.cell;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            drawn.reset();
            continue;
        }
        let mut symbol = String::from(cell.c);
        symbol.extend(cell.zerowidth().into_iter().flatten());
        drawn.set_symbol(&symbol);
        drawn.set_style(style(cell.fg, cell.bg, cell.flags, cell.underline_color()));
    }
}

fn style(fg: Colour, bg: Colour, flags: Flags, underline: Option<Colour>) -> Style {
    let (fg, dim_fg) = colour(fg);
    let (bg, _) = colour(bg);
    let mut style = Style::new().fg(fg).bg(bg);
    if let Some(underline) = underline {
        style = style.underline_color(colour(underline).0);
    }
    let modifiers = [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::HIDDEN, Modifier::HIDDEN),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ];
    for (flag, modifier) in modifiers {
        if flags.intersects(flag) {
            style = style.add_modifier(modifier);
        }
    }
    if dim_fg {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

const _: () = assert!(NamedColor::Black as usize == 0);
const _: () = assert!(NamedColor::BrightWhite as usize == 15);
const _: () = assert!(
    NamedColor::DimWhite as usize - NamedColor::DimBlack as usize
        == NamedColor::White as usize - NamedColor::Black as usize
);

fn colour(colour: Colour) -> (Color, bool) {
    match colour {
        Colour::Spec(rgb) => (Color::Rgb(rgb.r, rgb.g, rgb.b), false),
        Colour::Indexed(index) => (Color::Indexed(index), false),
        Colour::Named(named) => {
            let index = named as usize;
            if let Ok(index @ 0..=15) = u8::try_from(index) {
                return (Color::Indexed(index), false);
            }
            let dim = NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize;
            if dim.contains(&index) {
                let index = u8::try_from(index - NamedColor::DimBlack as usize)
                    .expect("one of the eight dim colours");
                return (Color::Indexed(index), true);
            }
            (Color::Reset, named == NamedColor::DimForeground)
        },
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;

    fn drawn(columns: u16, lines: u16, bytes: &[u8]) -> Buffer {
        let dimensions = TermSize::new(usize::from(columns), usize::from(lines));
        let mut screen = Term::new(Config::default(), &dimensions, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, bytes);
        let mut buffer = Buffer::empty(Rect::new(0, 0, columns + 4, lines + 2));
        show(&screen, &mut buffer, Rect::new(2, 1, columns, lines), 0);
        buffer
    }

    fn line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
    }

    #[test]
    fn the_screen_is_drawn_where_its_area_starts() {
        let buffer = drawn(6, 2, b"ab\r\ncd");
        assert_eq!(line(&buffer, 1), "  ab      ");
        assert_eq!(line(&buffer, 2), "  cd      ");
    }

    #[test]
    fn a_wide_character_takes_two_columns_and_draws_once() {
        let buffer = drawn(4, 1, "日x".as_bytes());
        assert_eq!(buffer[(2, 1)].symbol(), "日");
        assert_eq!(buffer[(4, 1)].symbol(), "x");
    }

    #[test]
    fn the_colours_a_terminal_names_are_left_to_its_theme() {
        let buffer = drawn(4, 1, b"\x1b[31;44ma\x1b[38;5;200mb\x1b[38;2;1;2;3mc\x1b[0md");
        assert_eq!(buffer[(2, 1)].style().fg, Some(Color::Indexed(1)));
        assert_eq!(buffer[(2, 1)].style().bg, Some(Color::Indexed(4)));
        assert_eq!(buffer[(3, 1)].style().fg, Some(Color::Indexed(200)));
        assert_eq!(buffer[(4, 1)].style().fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(buffer[(5, 1)].style().fg, Some(Color::Reset));
    }

    #[test]
    fn bold_and_underline_carry_over() {
        let buffer = drawn(2, 1, b"\x1b[1;4ma");
        let modifiers = buffer[(2, 1)].style().add_modifier;
        assert!(modifiers.contains(Modifier::BOLD | Modifier::UNDERLINED));
    }
}
