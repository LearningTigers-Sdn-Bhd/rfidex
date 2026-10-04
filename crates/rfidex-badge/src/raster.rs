//! Paint a planned badge into a grey bitmap.

use image::{GrayImage, Luma};
use qrcode::{Color as QrColor, EcLevel, QrCode};

use crate::layout::{plan, Draw, Layout, Ticket};
use crate::text::Fonts;

/// The QR quiet zone, in modules (event-printing used 2).
const QUIET: usize = 2;

/// Render one badge at `dpi`. White paper, pure black ink.
pub fn render(layout: &Layout, ticket: &Ticket, fonts: &Fonts, dpi: f64) -> GrayImage {
    let planned = plan(layout, ticket, fonts);
    let k = dpi / 72.0;
    let width = (planned.width_pt * k).round().max(1.0) as u32;
    let height = (planned.height_pt * k).round().max(1.0) as u32;
    let mut image = GrayImage::from_pixel(width, height, Luma([255]));
    for draw in &planned.draws {
        match draw {
            Draw::Text {
                text,
                bold,
                size_pt,
                center_x,
                baseline_y,
            } => fonts.draw(
                &mut image,
                text,
                *bold,
                size_pt * k,
                center_x * k,
                baseline_y * k,
            ),
            Draw::Qr { x, y, size } => {
                draw_qr(&mut image, &ticket.ticket_id, x * k, y * k, size * k)
            }
        }
    }
    image
}

fn draw_qr(image: &mut GrayImage, data: &str, x: f64, y: f64, size: f64) {
    let Ok(code) = QrCode::with_error_correction_level(data, EcLevel::M) else {
        return;
    };
    let modules = code.width();
    let cell = size / (modules + 2 * QUIET) as f64;
    let colors = code.to_colors();
    for row in 0..modules {
        for col in 0..modules {
            if colors[row * modules + col] != QrColor::Dark {
                continue;
            }
            let edge = |i: usize| (i + QUIET) as f64 * cell;
            let (x0, x1) = ((x + edge(col)).round(), (x + edge(col + 1)).round());
            let (y0, y1) = ((y + edge(row)).round(), (y + edge(row + 1)).round());
            for py in (y0.max(0.0) as u32)..(y1.max(0.0) as u32).min(image.height()) {
                for px in (x0.max(0.0) as u32)..(x1.max(0.0) as u32).min(image.width()) {
                    image.put_pixel(px, py, Luma([0]));
                }
            }
        }
    }
}

/// The badge as PNG bytes, for the on-screen preview.
pub fn to_png(image: &GrayImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("a grey image encodes as PNG in memory");
    bytes
}
