//! A pane's view in the protocol's words: a frame of its rows, its cursor and
//! the modes a client mirrors.

use crate::api;
use crate::asked::Size;
use crate::id::PaneId;
use crate::screen::{Cell, Cursor, CursorShape, Glyph, Line, Style, StyleColor, Underline, View};

/// What a frame says of the pane besides its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub clusters: bool,
    pub tracks: bool,
    pub back: usize,
    pub scrollback: usize,
}

/// The frame of pane `pane` that shows `view`, of `size`, to a client that
/// last got `before`: the rows that changed since, or all of them where it
/// got none of this size.
#[must_use]
pub fn said(
    pane: PaneId,
    view: &View,
    before: Option<(Size, &View)>,
    size: Size,
    flags: Flags,
) -> api::Frame {
    let rows = match before {
        Some((before_size, before)) if before_size == size => {
            assert_eq!(view.lines.len(), before.lines.len(), "two views of {size:?}");
            let rows = view
                .lines
                .iter()
                .zip(&before.lines)
                .enumerate()
                .filter(|(_, (line, before))| line != before)
                .map(|(index, (line, _))| api::Row {
                    index: u32::try_from(index).expect("a pane's rows are counted in a u16"),
                    line: Some(line_said(line)),
                })
                .collect();
            api::frame::Rows::Changed(api::Changed { rows })
        },
        _ => api::frame::Rows::Whole(api::Whole {
            lines: view.lines.iter().map(line_said).collect(),
        }),
    };
    let count =
        |count: usize| u32::try_from(count).expect("a pane's scrollback is counted in a u32");
    api::Frame {
        pane: pane.said(),
        size: Some(size.said()),
        rows: Some(rows),
        cursor: view.cursor.map(cursor_said),
        clusters: flags.clusters,
        mouse_tracking: flags.tracks,
        scrolled_back: count(flags.back),
        scrollback: count(flags.scrollback),
    }
}

fn line_said(line: &Line) -> api::Line {
    api::Line {
        glyphs: line.cells().iter().map(glyph_said).collect(),
        wraps_wide: line.wraps_wide(),
    }
}

fn glyph_said(cell: &Cell) -> api::Glyph {
    let shows = match &cell.glyph {
        Glyph::Blank => api::glyph::Shows::Blank(api::Blank {}),
        Glyph::Narrow(grapheme) => {
            api::glyph::Shows::Narrow(api::Narrow { grapheme: grapheme.clone() })
        },
        Glyph::Wide(grapheme) => api::glyph::Shows::Wide(api::Wide { grapheme: grapheme.clone() }),
    };
    let style = (cell.style != Style::default()).then(|| style_said(&cell.style));
    api::Glyph { shows: Some(shows), style }
}

fn style_said(style: &Style) -> api::Style {
    let underline = match style.underline {
        Underline::Single => api::Underline::Single,
        Underline::Double => api::Underline::Double,
        Underline::Curly => api::Underline::Curly,
        Underline::Dotted => api::Underline::Dotted,
        Underline::Dashed => api::Underline::Dashed,
        _ => api::Underline::None,
    };
    api::Style {
        foreground: colour_said(style.fg_color),
        background: colour_said(style.bg_color),
        underline_colour: colour_said(style.underline_color),
        bold: style.bold,
        italic: style.italic,
        faint: style.faint,
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        underline: underline.into(),
    }
}

fn colour_said(colour: StyleColor) -> Option<api::Colour> {
    let is = match colour {
        StyleColor::None => return None,
        StyleColor::Palette(index) => api::colour::Is::Palette(index.0.into()),
        StyleColor::Rgb(rgb) => api::colour::Is::Rgb(api::Rgb {
            red: rgb.r.into(),
            green: rgb.g.into(),
            blue: rgb.b.into(),
        }),
    };
    Some(api::Colour { is: Some(is) })
}

fn cursor_said(cursor: Cursor) -> api::Cursor {
    let shape = match cursor.shape {
        CursorShape::Bar => api::CursorShape::Bar,
        CursorShape::Underline => api::CursorShape::Underline,
        CursorShape::BlockHollow => api::CursorShape::BlockHollow,
        _ => api::CursorShape::Block,
    };
    api::Cursor {
        row: cursor.row.into(),
        column: cursor.column.into(),
        shape: shape.into(),
        blinking: cursor.blinking,
    }
}
