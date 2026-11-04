//! A picture as terminal cells: each cell is `▀` with the upper pixel as its
//! foreground and the lower one as its background, so `cols × rows` cells
//! show `cols × 2·rows` pixels. Works in any truecolor terminal, no graphics
//! protocol needed.

use image::{DynamicImage, GenericImageView, imageops::FilterType};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

/// The picture fitted into at most `max_cols × max_rows` cells, keeping its
/// shape (cells are about twice as tall as wide).
pub fn lines(img: &DynamicImage, max_cols: u16, max_rows: u16) -> Vec<Line<'static>> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || max_cols == 0 || max_rows == 0 {
        return Vec::new();
    }
    let max_px_w = u32::from(max_cols);
    let max_px_h = u32::from(max_rows) * 2;
    let scale = (max_px_w as f64 / w as f64).min(max_px_h as f64 / h as f64);
    let tw = ((w as f64 * scale).round() as u32).clamp(1, max_px_w);
    let th = ((h as f64 * scale).round() as u32).clamp(2, max_px_h);
    let small = img.resize_exact(tw, th, FilterType::Triangle).to_rgb8();
    let mut out = Vec::with_capacity(th.div_ceil(2) as usize);
    for y in (0..th).step_by(2) {
        let mut spans = Vec::with_capacity(tw as usize);
        for x in 0..tw {
            let top = small.get_pixel(x, y);
            let bottom = if y + 1 < th {
                *small.get_pixel(x, y + 1)
            } else {
                *top
            };
            spans.push(Span::styled(
                "▀",
                Style::default()
                    .fg(Color::Rgb(top[0], top[1], top[2]))
                    .bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
            ));
        }
        out.push(Line::from(spans));
    }
    out
}

/// Decodes PNG/JPEG bytes and draws them (`None` when they don't decode).
pub fn from_bytes(bytes: &[u8], max_cols: u16, max_rows: u16) -> Option<Vec<Line<'static>>> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(lines(&img, max_cols, max_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_its_box_with_two_pixels_per_cell() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(200, 100, |x, _| {
            if x < 100 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        }));
        let l = lines(&img, 20, 10);
        // 200×100 into 20 wide → 20×10 pixels → 5 rows of cells.
        assert_eq!(l.len(), 5);
        assert_eq!(l[0].spans.len(), 20);
        assert_eq!(l[0].spans[0].style.fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(l[0].spans[19].style.fg, Some(Color::Rgb(0, 0, 255)));
    }
}
