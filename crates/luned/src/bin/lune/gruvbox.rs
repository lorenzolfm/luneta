use ratatui::style::Color;

pub const BG0: Color = Color::Rgb(0x28, 0x28, 0x28);
pub const FG1: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
pub const BG1: Color = Color::Rgb(0x3c, 0x38, 0x36);
pub const BG2: Color = Color::Rgb(0x50, 0x49, 0x45);
pub const BG3: Color = Color::Rgb(0x66, 0x5c, 0x54);
pub const RED: Color = Color::Rgb(0xfb, 0x49, 0x34);
pub const GREEN: Color = Color::Rgb(0xb8, 0xbb, 0x26);
pub const YELLOW: Color = Color::Rgb(0xfa, 0xbd, 0x2f);
pub const BLUE: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const PURPLE: Color = Color::Rgb(0xd3, 0x86, 0x9b);
pub const AQUA: Color = Color::Rgb(0x8e, 0xc0, 0x7c);
pub const ORANGE: Color = Color::Rgb(0xfe, 0x80, 0x19);
pub const GREY: Color = Color::Rgb(0x92, 0x83, 0x74);

#[cfg(test)]
pub const ALL: [Color; 13] =
    [BG0, FG1, BG1, BG2, BG3, RED, GREEN, YELLOW, BLUE, PURPLE, AQUA, ORANGE, GREY];
