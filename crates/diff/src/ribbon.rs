use std::num::{NonZeroU16, NonZeroU32};

use crate::layout::{Ribbon, RibbonKind};

pub const GUTTER_COLS: u16 = 6;

const CELL_WIDTH_PX_MIN: u16 = 4;
const CELL_WIDTH_PX_MAX: u16 = 48;
const CELL_HEIGHT_PX_MIN: u16 = 8;
const CELL_HEIGHT_PX_MAX: u16 = 96;

const IMAGE_SIDE_MAX: u32 = 4096;

const _: () = assert!(GUTTER_COLS > 0);
const _: () = assert!(CELL_WIDTH_PX_MIN > 0 && CELL_WIDTH_PX_MIN <= CELL_WIDTH_PX_MAX);
const _: () = assert!(CELL_HEIGHT_PX_MIN > 0 && CELL_HEIGHT_PX_MIN <= CELL_HEIGHT_PX_MAX);
const _: () = assert!(GUTTER_COLS as u32 * CELL_WIDTH_PX_MAX as u32 <= IMAGE_SIDE_MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    #[must_use]
    pub fn lerp(self, other: Rgb, ratio: f32) -> Rgb {
        let clamped = ratio.clamp(0.0, 1.0);
        let mix = |source: u8, target: u8| {
            let value = f32::from(source) + (f32::from(target) - f32::from(source)) * clamped;
            value.round().clamp(0.0, 255.0) as u8
        };
        Rgb {
            r: mix(self.r, other.r),
            g: mix(self.g, other.g),
            b: mix(self.b, other.b),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RibbonPalette {
    pub red_fill: Rgb,
    pub green_fill: Rgb,
    pub red_edge: Rgb,
    pub green_edge: Rgb,
    pub fill_alpha: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RibbonImage {
    width: NonZeroU32,
    height: NonZeroU32,
    rgba: Vec<u8>,
}

impl RibbonImage {
    #[must_use]
    pub const fn width(&self) -> NonZeroU32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> NonZeroU32 {
        self.height
    }

    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RibbonCell {
    pub glyph: char,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
}

impl RibbonCell {
    pub const EMPTY: RibbonCell = RibbonCell {
        glyph: ' ',
        fg: None,
        bg: None,
    };
}

fn smoothstep(ratio: f32) -> f32 {
    let clamped = ratio.clamp(0.0, 1.0);
    let result = clamped * clamped * (3.0 - 2.0 * clamped);
    assert!((0.0..=1.0).contains(&result), "smoothstep stays in [0, 1]");
    result
}

fn ribbon_color(kind: RibbonKind, ratio: f32, palette: RibbonPalette) -> Rgb {
    match kind {
        RibbonKind::Modified => palette.red_fill.lerp(palette.green_fill, ratio),
        RibbonKind::Inserted => palette.green_fill,
        RibbonKind::Deleted => palette.red_fill,
    }
}

fn edge_color(kind: RibbonKind, ratio: f32, palette: RibbonPalette) -> Rgb {
    match kind {
        RibbonKind::Modified => palette.red_edge.lerp(palette.green_edge, ratio),
        RibbonKind::Inserted => palette.green_edge,
        RibbonKind::Deleted => palette.red_edge,
    }
}

fn relative_row(row: usize, scroll: usize) -> f32 {
    if row >= scroll {
        (row - scroll) as f32
    } else {
        -((scroll - row) as f32)
    }
}

fn visible_ribbon(
    ribbon: &Ribbon,
    scroll: usize,
    visible_rows: usize,
) -> Option<(f32, f32, f32, f32)> {
    let (left, right) = ribbon.shape.spans();
    let min_row = left.start.min(right.start);
    let max_row = left.end.max(right.end);
    let view_end = scroll.saturating_add(visible_rows);
    if max_row < scroll || min_row > view_end {
        return None;
    }
    Some((
        relative_row(left.start, scroll),
        relative_row(left.end, scroll),
        relative_row(right.start, scroll),
        relative_row(right.end, scroll),
    ))
}

#[must_use]
pub fn clamped_cell_px(cell_px: (NonZeroU16, NonZeroU16)) -> (NonZeroU16, NonZeroU16) {
    let cell_width = NonZeroU16::new(cell_px.0.get().clamp(CELL_WIDTH_PX_MIN, CELL_WIDTH_PX_MAX))
        .expect("CELL_WIDTH_PX_MIN is non-zero");
    let cell_height =
        NonZeroU16::new(cell_px.1.get().clamp(CELL_HEIGHT_PX_MIN, CELL_HEIGHT_PX_MAX))
            .expect("CELL_HEIGHT_PX_MIN is non-zero");
    (cell_width, cell_height)
}

#[must_use]
pub fn rasterize_rgba(
    ribbons: &[Ribbon],
    scroll: usize,
    visible_rows: usize,
    gutter_cols: u16,
    cell_px: (NonZeroU16, NonZeroU16),
    palette: RibbonPalette,
) -> Option<RibbonImage> {
    if visible_rows == 0 || gutter_cols == 0 {
        return None;
    }
    let (cell_width, cell_height) = clamped_cell_px(cell_px);
    let width = NonZeroU32::new(
        (u32::from(gutter_cols) * u32::from(cell_width.get())).min(IMAGE_SIDE_MAX),
    )?;
    let visible_rows_u32 = u32::try_from(visible_rows).unwrap_or(u32::MAX);
    let height = NonZeroU32::new(
        visible_rows_u32
            .saturating_mul(u32::from(cell_height.get()))
            .min(IMAGE_SIDE_MAX),
    )?;

    let active: Vec<(f32, f32, f32, f32, RibbonKind)> = ribbons
        .iter()
        .filter_map(|ribbon| {
            let (left_start, left_end, right_start, right_end) =
                visible_ribbon(ribbon, scroll, visible_rows)?;
            Some((left_start, left_end, right_start, right_end, ribbon.shape.kind()))
        })
        .collect();
    if active.is_empty() {
        return None;
    }
    let width_u32 = width.get();
    let height_u32 = height.get();
    let width_usize = usize::try_from(width_u32).expect("bounded width fits usize");
    let height_usize = usize::try_from(height_u32).expect("bounded height fits usize");
    let expected_bytes = width_usize
        .checked_mul(height_usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .expect("bounded RGBA buffer size fits usize");
    let mut rgba = vec![0u8; expected_bytes];
    let row_height = height_u32 as f32 / visible_rows as f32;
    let width_inverse = if width_u32 > 1 {
        1.0 / (width_u32 - 1) as f32
    } else {
        0.0
    };
    let alpha_scale = f32::from(palette.fill_alpha) / 255.0;
    let max_y = height_u32 as f32;

    for x in 0..width_u32 {
        let ratio = x as f32 * width_inverse;
        let curve = smoothstep(ratio);
        for &(left_start, left_end, right_start, right_end, kind) in &active {
            let y_top = (left_start + (right_start - left_start) * curve) * row_height;
            let y_bottom = (left_end + (right_end - left_end) * curve) * row_height;
            let (top, bottom) = if y_bottom >= y_top {
                (y_top, y_bottom)
            } else {
                (y_bottom, y_top)
            };
            let min_span = (bottom - top).max(1.2);
            let draw_top = top - 0.5 * (min_span - (bottom - top));
            let draw_bottom = draw_top + min_span;

            let py_start = (draw_top.floor() - 1.0).clamp(0.0, max_y) as u32;
            let py_end = (draw_bottom.ceil() + 1.0).clamp(0.0, max_y) as u32;
            assert!(py_start <= py_end, "pixel y range is ordered");
            assert!(py_end <= height_u32, "pixel y range fits image height");

            let fill = ribbon_color(kind, ratio, palette);
            let edge = edge_color(kind, ratio, palette);

            for y in py_start..py_end {
                let mut covered = 0u32;
                let mut on_edge = false;
                for subpixel in 0..4u32 {
                    let sample_y = y as f32 + (subpixel as f32 + 0.5) * 0.25;
                    if sample_y >= draw_top && sample_y <= draw_bottom {
                        covered += 1;
                        if (sample_y - draw_top) < 1.1 || (draw_bottom - sample_y) < 1.1 {
                            on_edge = true;
                        }
                    }
                }
                if covered == 0 {
                    continue;
                }
                let coverage = covered as f32 * 0.25;
                let color = if on_edge { edge } else { fill };
                let alpha_weight = if on_edge {
                    (alpha_scale * 1.15).min(1.0)
                } else {
                    alpha_scale
                };
                let source_alpha = (coverage * alpha_weight).clamp(0.0, 1.0);
                let y_usize = usize::try_from(y).expect("y < height fits usize");
                let x_usize = usize::try_from(x).expect("x < width fits usize");
                let offset = (y_usize * width_usize + x_usize) * 4;
                blend_over(&mut rgba[offset..offset + 4], color, source_alpha);
            }
        }
    }
    assert_eq!(
        rgba.len(),
        expected_bytes,
        "rasterized RGBA payload matches width * height * 4"
    );
    Some(RibbonImage {
        width,
        height,
        rgba,
    })
}

fn blend_over(target: &mut [u8], source: Rgb, source_alpha: f32) {
    assert_eq!(target.len(), 4, "RGBA pixel slice is 4 bytes");
    assert!(
        (0.0..=1.0).contains(&source_alpha),
        "source alpha stays in [0, 1]"
    );
    let target_alpha = f32::from(target[3]) / 255.0;
    let output_alpha = source_alpha + target_alpha * (1.0 - source_alpha);
    if output_alpha <= 0.0001 {
        return;
    }
    let blend_channel = |src: u8, dst: u8| {
        let value = (f32::from(src) * source_alpha
            + f32::from(dst) * target_alpha * (1.0 - source_alpha))
            / output_alpha;
        value.round().clamp(0.0, 255.0) as u8
    };
    target[0] = blend_channel(source.r, target[0]);
    target[1] = blend_channel(source.g, target[1]);
    target[2] = blend_channel(source.b, target[2]);
    target[3] = (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[must_use]
pub fn rasterize_cells(
    ribbons: &[Ribbon],
    scroll: usize,
    visible_rows: usize,
    gutter_cols: u16,
    palette: RibbonPalette,
) -> Vec<Vec<RibbonCell>> {
    let cols = usize::from(gutter_cols);
    let mut grid = vec![vec![RibbonCell::EMPTY; cols]; visible_rows];
    if visible_rows == 0 || cols == 0 {
        return grid;
    }
    let active: Vec<(f32, f32, f32, f32, RibbonKind)> = ribbons
        .iter()
        .filter_map(|ribbon| {
            let (left_start, left_end, right_start, right_end) =
                visible_ribbon(ribbon, scroll, visible_rows)?;
            Some((left_start, left_end, right_start, right_end, ribbon.shape.kind()))
        })
        .collect();
    if active.is_empty() {
        return grid;
    }
    let denom = if cols > 1 { (cols - 1) as f32 } else { 1.0 };
    for (row, grid_row) in grid.iter_mut().enumerate() {
        let top_y = row as f32 + 0.25;
        let bottom_y = row as f32 + 0.75;
        for (col, cell) in grid_row.iter_mut().enumerate() {
            let ratio = (col as f32 + 0.5) / (denom + 1.0);
            let curve = smoothstep(ratio);
            let mut top_color = None;
            let mut bottom_color = None;
            for &(left_start, left_end, right_start, right_end, kind) in &active {
                let y_start = left_start + (right_start - left_start) * curve;
                let y_end = left_end + (right_end - left_end) * curve;
                let (lo, hi) = if y_end >= y_start {
                    (y_start, y_end)
                } else {
                    (y_end, y_start)
                };
                let span = (hi - lo).max(0.24);
                let midpoint = f32::midpoint(lo, hi);
                let lo = midpoint - 0.5 * span;
                let hi = midpoint + 0.5 * span;
                let color = ribbon_color(kind, ratio, palette);
                if top_y >= lo && top_y <= hi {
                    top_color = Some(color);
                }
                if bottom_y >= lo && bottom_y <= hi {
                    bottom_color = Some(color);
                }
            }
            *cell = match (top_color, bottom_color) {
                (None, None) => RibbonCell::EMPTY,
                (Some(top), None) => RibbonCell {
                    glyph: '▀',
                    fg: Some(top),
                    bg: None,
                },
                (None, Some(bottom)) => RibbonCell {
                    glyph: '▄',
                    fg: Some(bottom),
                    bg: None,
                },
                (Some(top), Some(bottom)) if top == bottom => RibbonCell {
                    glyph: '█',
                    fg: Some(top),
                    bg: None,
                },
                (Some(top), Some(bottom)) => RibbonCell {
                    glyph: '▀',
                    fg: Some(top),
                    bg: Some(bottom),
                },
            };
        }
    }
    assert_eq!(grid.len(), visible_rows, "cell grid matches visible rows");
    grid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{RibbonShape, RowSpan};

    fn palette() -> RibbonPalette {
        RibbonPalette {
            red_fill: Rgb::new(0x52, 0x2d, 0x3b),
            green_fill: Rgb::new(0x2b, 0x4b, 0x3a),
            red_edge: Rgb::new(0x88, 0x49, 0x5d),
            green_edge: Rgb::new(0x49, 0x80, 0x62),
            fill_alpha: 215,
        }
    }

    fn px(w: u16, h: u16) -> (NonZeroU16, NonZeroU16) {
        (NonZeroU16::new(w).unwrap(), NonZeroU16::new(h).unwrap())
    }

    #[test]
    fn rgba_rasterizer_paints_curved_ribbon_with_left_to_right_gradient() {
        let ribbons = vec![Ribbon {
            shape: RibbonShape::Modified {
                left: RowSpan::from_range(1, 4).unwrap(),
                right: RowSpan::from_range(1, 2).unwrap(),
            },
            file: 0,
            hunk: 0,
        }];
        let image = rasterize_rgba(&ribbons, 0, 6, 6, px(8, 16), palette()).unwrap();
        assert_eq!((image.width().get(), image.height().get()), (48, 96));
        let w = image.width().get();
        let alpha_at = |x: u32, y: u32| image.rgba()[((y * w + x) * 4 + 3) as usize];
        assert_eq!(alpha_at(0, 4), 0);
        assert!(alpha_at(0, 24) > 0);
        assert!(alpha_at(47, 24) > 0);
        assert_eq!(alpha_at(47, 56), 0);
        let left_r = image.rgba()[(24 * w * 4) as usize];
        let right_g = image.rgba()[((24 * w + 47) * 4 + 1) as usize];
        assert!(left_r > right_g / 2);
    }

    #[test]
    fn pure_insertion_pinches_at_left_seam_and_expands_right() {
        let ribbons = vec![Ribbon {
            shape: RibbonShape::Inserted {
                left_seam: 3,
                right: RowSpan::from_range(2, 4).unwrap(),
            },
            file: 0,
            hunk: 0,
        }];
        let cells = rasterize_cells(&ribbons, 0, 6, 6, palette());
        assert_eq!(cells[0][0], RibbonCell::EMPTY);
        assert_ne!(cells[2][5], RibbonCell::EMPTY);
        assert_ne!(cells[3][5], RibbonCell::EMPTY);
    }

    #[test]
    fn offscreen_and_tall_ribbon_ranges_clamp_without_overflow() {
        let ribbons = vec![Ribbon {
            shape: RibbonShape::Modified {
                left: RowSpan::from_range(0, 200_000).unwrap(),
                right: RowSpan::from_range(0, 200_000).unwrap(),
            },
            file: 0,
            hunk: 0,
        }];
        let image = rasterize_rgba(&ribbons, 100_000, 30, 6, px(8, 16), palette()).unwrap();
        assert_eq!(
            image.rgba().len(),
            usize::try_from(image.width().get() * image.height().get() * 4).unwrap()
        );
    }
}
