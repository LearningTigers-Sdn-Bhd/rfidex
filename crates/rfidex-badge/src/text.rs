//! Real text: shaping, fallback fonts and glyph drawing.
//!
//! Arial is the primary face, so wrapping matches the Helvetica-based badges
//! event-printing made. A glyph Arial lacks (Chinese, Tamil, Thai, ...) is
//! shaped with the system's fallback fonts. Starting the font system scans the
//! installed fonts, which takes a moment: make one `Fonts` and keep it.

use std::cell::RefCell;
use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use image::GrayImage;

use crate::layout::Measure;

pub struct Fonts {
    system: RefCell<FontSystem>,
    cache: RefCell<SwashCache>,
    widths: RefCell<HashMap<(String, bool, u32), f64>>,
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts::new()
    }
}

impl Fonts {
    pub fn new() -> Fonts {
        Fonts {
            system: RefCell::new(FontSystem::new()),
            cache: RefCell::new(SwashCache::new()),
            widths: RefCell::new(HashMap::new()),
        }
    }

    fn shaped(system: &mut FontSystem, text: &str, bold: bool, size: f32) -> Buffer {
        let mut buffer = Buffer::new(system, Metrics::new(size, size * 1.25));
        buffer.set_size(None, None);
        let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
        let attrs = Attrs::new().family(Family::Name("Arial")).weight(weight);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(system, false);
        buffer
    }

    /// Paint `text` black, centred on `center_x`, sitting on `baseline_y`.
    /// Coordinates are pixels. Edges are anti-aliased; the caller thresholds.
    pub fn draw(
        &self,
        canvas: &mut GrayImage,
        text: &str,
        bold: bool,
        size_px: f64,
        center_x: f64,
        baseline_y: f64,
    ) {
        let mut system = self.system.borrow_mut();
        let mut cache = self.cache.borrow_mut();
        let mut buffer = Self::shaped(&mut system, text, bold, size_px as f32);
        let Some((line_w, line_y)) = buffer
            .layout_runs()
            .next()
            .map(|run| (run.line_w as f64, run.line_y as f64))
        else {
            return;
        };
        let left = (center_x - line_w / 2.0).round() as i32;
        let top = (baseline_y - line_y).round() as i32;
        let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
        buffer.draw(
            &mut system,
            &mut cache,
            Color::rgb(0, 0, 0),
            |x, y, w, h, color| {
                let alpha = color.a() as u32;
                if alpha == 0 {
                    return;
                }
                for dy in 0..h as i32 {
                    for dx in 0..w as i32 {
                        let (px, py) = (left + x + dx, top + y + dy);
                        if px < 0 || py < 0 || px >= cw || py >= ch {
                            continue;
                        }
                        let pixel = canvas.get_pixel_mut(px as u32, py as u32);
                        pixel.0[0] = (pixel.0[0] as u32 * (255 - alpha) / 255) as u8;
                    }
                }
            },
        );
    }
}

impl Measure for Fonts {
    fn width(&self, text: &str, bold: bool, size_pt: f64) -> f64 {
        let key = (text.to_string(), bold, (size_pt * 100.0).round() as u32);
        if let Some(w) = self.widths.borrow().get(&key) {
            return *w;
        }
        let mut system = self.system.borrow_mut();
        let buffer = Self::shaped(&mut system, text, bold, size_pt as f32);
        let width = buffer
            .layout_runs()
            .map(|run| run.line_w as f64)
            .fold(0.0, f64::max);
        self.widths.borrow_mut().insert(key, width);
        width
    }
}
