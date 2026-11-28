use ratatui::style::Color;

pub const INK: Color = Color::Rgb(0x1c, 0x1b, 0x29);
pub const TEXT: Color = Color::Rgb(0xe2, 0xe0, 0xef);
pub const SURFACE: Color = Color::Rgb(0x2d, 0x2b, 0x3d);
pub const LIFTED: Color = Color::Rgb(0x3b, 0x38, 0x53);
pub const RAISED: Color = Color::Rgb(0x4c, 0x48, 0x6a);
pub const RED: Color = Color::Rgb(0xff, 0x8f, 0xa3);
pub const GREEN: Color = Color::Rgb(0xa8, 0xe6, 0xa3);
pub const YELLOW: Color = Color::Rgb(0xf7, 0xd8, 0x8b);
pub const BLUE: Color = Color::Rgb(0x8c, 0xb4, 0xff);
pub const MAUVE: Color = Color::Rgb(0xc6, 0xa7, 0xff);
pub const CYAN: Color = Color::Rgb(0x86, 0xe1, 0xfc);
pub const PEACH: Color = Color::Rgb(0xf8, 0xb4, 0x8c);
pub const MUTE: Color = Color::Rgb(0x7f, 0x7b, 0x99);

#[cfg(test)]
pub const ALL: [Color; 13] = [
    INK, TEXT, SURFACE, LIFTED, RAISED, RED, GREEN, YELLOW, BLUE, MAUVE, CYAN, PEACH, MUTE,
];
