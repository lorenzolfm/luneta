//! The colours the sidebar draws in: gruvbox's dark ones, whatever the
//! terminal's own are, on the terminal's background. Its accents are the
//! bright ones, which gruvbox gives a dark background.

use ratatui::style::Color;

/// Text with no colour of its own.
pub const FG1: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
/// The softer of the highlight's two backgrounds.
pub const BG1: Color = Color::Rgb(0x3c, 0x38, 0x36);
/// The plainer of the highlight's two backgrounds.
pub const BG2: Color = Color::Rgb(0x50, 0x49, 0x45);
pub const RED: Color = Color::Rgb(0xfb, 0x49, 0x34);
pub const GREEN: Color = Color::Rgb(0xb8, 0xbb, 0x26);
pub const YELLOW: Color = Color::Rgb(0xfa, 0xbd, 0x2f);
pub const BLUE: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const PURPLE: Color = Color::Rgb(0xd3, 0x86, 0x9b);
pub const AQUA: Color = Color::Rgb(0x8e, 0xc0, 0x7c);
pub const ORANGE: Color = Color::Rgb(0xfe, 0x80, 0x19);
/// gruvbox's gray.
pub const GREY: Color = Color::Rgb(0x92, 0x83, 0x74);

/// Every colour the sidebar draws in, which its tests hold each cell to: a
/// colour named at a call site, as `Color::Red`, would be the terminal's.
#[cfg(test)]
pub const ALL: [Color; 11] =
    [FG1, BG1, BG2, RED, GREEN, YELLOW, BLUE, PURPLE, AQUA, ORANGE, GREY];
