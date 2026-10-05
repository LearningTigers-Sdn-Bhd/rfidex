//! From a rendered badge to paper.
//!
//! `prepare` is the portable half: it turns the grey badge into the exact
//! 1-bit bitmap the printer driver is given, the same steps event-printing
//! took (optional thickening, a turn, fit to the printable area, a hard
//! black/white threshold so the driver never halftones the edges into dots).
//! The Windows half is in `windows.rs`.

use image::{imageops, GrayImage, Luma};

use crate::layout::Paper;

/// Anything lighter than this is paper. Matches event-printing.
const WHITE_FROM: u8 = 220;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintError {
    /// Not a Windows PC: there is no printer to talk to.
    Unsupported,
    /// Disabled or invalidated before a job reached the spooler.
    Cancelled,
    /// No printer is installed or chosen.
    NoPrinter,
    /// The printer could not be opened (unplugged, renamed, removed).
    PrinterUnavailable,
    /// The printer refused the job or the page.
    Rejected,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrinterList {
    pub names: Vec<String>,
    pub default: Option<String>,
}

/// A 1-bit-per-pixel picture, top row first, 1 = white, rows padded to four
/// bytes, ready for a bottom-up-free DIB, plus where it sits on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub bits: Vec<u8>,
}

/// Grow dark strokes by one pixel on every side. Direct-thermal stock drops
/// dots, and fatter strokes survive that.
pub fn thicken(image: &GrayImage) -> GrayImage {
    let (w, h) = image.dimensions();
    GrayImage::from_fn(w, h, |x, y| {
        let mut darkest = 255u8;
        for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
            for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                darkest = darkest.min(image.get_pixel(nx, ny).0[0]);
            }
        }
        Luma([darkest])
    })
}

/// Turn the badge into what the printer gets, centred in the `target_w` x
/// `target_h` printable area at the largest size that keeps its shape.
pub fn prepare(
    badge: &GrayImage,
    target_w: u32,
    target_h: u32,
    thicker: bool,
    rotate_90: bool,
) -> Prepared {
    let source = if thicker {
        thicken(badge)
    } else {
        badge.clone()
    };
    let turned = if rotate_90 {
        imageops::rotate90(&source)
    } else {
        imageops::rotate180(&source)
    };
    let scale =
        (target_w as f64 / turned.width() as f64).min(target_h as f64 / turned.height() as f64);
    let width = ((turned.width() as f64 * scale) as u32).max(1);
    let height = ((turned.height() as f64 * scale) as u32).max(1);
    let fitted = if (width, height) == turned.dimensions() {
        turned
    } else {
        imageops::resize(&turned, width, height, imageops::FilterType::Lanczos3)
    };
    let stride = (width as usize).div_ceil(32) * 4;
    let mut bits = vec![0u8; stride * height as usize];
    for (x, y, pixel) in fitted.enumerate_pixels() {
        if pixel.0[0] >= WHITE_FROM {
            bits[y as usize * stride + (x as usize / 8)] |= 0x80 >> (x % 8);
        }
    }
    Prepared {
        x: (target_w.saturating_sub(width) / 2) as i32,
        y: (target_h.saturating_sub(height) / 2) as i32,
        width,
        height,
        bits,
    }
}

/// The resolution to render at: the printer's own when the thin-stroke fix is
/// on (so there is no resampling), otherwise event-printing's 200.
pub fn render_dpi(thermal: bool, device_dpi: i32) -> f64 {
    if !thermal {
        return 200.0;
    }
    if (100..=1200).contains(&device_dpi) {
        device_dpi as f64
    } else {
        203.0
    }
}

/// One badge to print. `document` names the job in the Windows print queue.
#[derive(Clone, Copy)]
pub struct Job<'a> {
    pub printer: &'a str,
    pub paper: &'a Paper,
    pub thermal: bool,
    pub rotate_90: bool,
    pub document: &'a str,
    pub submission: Option<&'a dyn Submission>,
}

/// The last eligibility check before a spooler document is started. The check
/// must not hold any lock while `start` runs: a driver can block there.
pub trait Submission: Send + Sync {
    fn submit(&self, start: &mut dyn FnMut() -> Result<(), PrintError>) -> Result<(), PrintError>;
}

impl Job<'_> {
    /// Physical stock dimensions; the readable badge layout stays unchanged.
    pub fn paper_size(&self) -> Paper {
        if self.rotate_90 {
            Paper {
                width_mm: self.paper.height_mm,
                height_mm: self.paper.width_mm,
            }
        } else {
            self.paper.clone()
        }
    }

    pub fn submit(
        &self,
        start: &mut dyn FnMut() -> Result<(), PrintError>,
    ) -> Result<(), PrintError> {
        match self.submission {
            Some(guard) => guard.submit(start),
            None => start(),
        }
    }
}

/// How badges reach paper. The real one is `SystemPrinting`; tests swap in a
/// recorder so no test ever needs a printer.
pub trait Printing: Send + Sync {
    /// The printers installed on this PC and which one is the default.
    fn printers(&self) -> Result<PrinterList, PrintError>;
    /// Print one badge. `render` is called once with the dpi to draw at.
    fn print(&self, job: &Job, render: &dyn Fn(f64) -> GrayImage) -> Result<(), PrintError>;
}

/// The Windows print system. Anywhere else it reports `Unsupported`.
#[derive(Debug, Default)]
pub struct SystemPrinting;

impl Printing for SystemPrinting {
    #[cfg(windows)]
    fn printers(&self) -> Result<PrinterList, PrintError> {
        crate::gdi::printers()
    }

    #[cfg(not(windows))]
    fn printers(&self) -> Result<PrinterList, PrintError> {
        Err(PrintError::Unsupported)
    }

    #[cfg(windows)]
    fn print(&self, job: &Job, render: &dyn Fn(f64) -> GrayImage) -> Result<(), PrintError> {
        crate::gdi::print(job, render)
    }

    #[cfg(not(windows))]
    fn print(&self, _job: &Job, _render: &dyn Fn(f64) -> GrayImage) -> Result<(), PrintError> {
        Err(PrintError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32) -> GrayImage {
        GrayImage::from_pixel(w, h, Luma([255]))
    }

    fn white_at(p: &Prepared, x: u32, y: u32) -> bool {
        let stride = (p.width as usize).div_ceil(32) * 4;
        p.bits[y as usize * stride + x as usize / 8] & (0x80 >> (x % 8)) != 0
    }

    #[test]
    fn thickening_grows_a_dot_to_a_square() {
        let mut image = blank(5, 5);
        image.put_pixel(2, 2, Luma([0]));
        let fat = thicken(&image);
        let dark = fat.pixels().filter(|p| p.0[0] == 0).count();
        assert_eq!(dark, 9);
        assert_eq!(fat.get_pixel(0, 0).0[0], 255);
    }

    #[test]
    fn the_badge_is_turned_half_way_round() {
        let mut image = blank(40, 20);
        image.put_pixel(0, 0, Luma([0])); // top left
        let p = prepare(&image, 40, 20, false, false);
        assert!(!white_at(&p, 39, 19), "the mark moved to the bottom right");
        assert!(white_at(&p, 0, 0));
    }

    #[test]
    fn sideways_roll_turns_the_whole_badge_without_shrinking() {
        let mut image = blank(100, 80);
        image.put_pixel(0, 0, Luma([0]));
        image.put_pixel(99, 79, Luma([0]));
        let p = prepare(&image, 80, 100, false, true);
        assert_eq!((p.width, p.height, p.x, p.y), (80, 100, 0, 0));
        assert!(!white_at(&p, 79, 0));
        assert!(!white_at(&p, 0, 99));
        assert!(white_at(&p, 0, 0));
    }

    #[test]
    fn it_fits_the_page_and_centres() {
        let p = prepare(&blank(800, 640), 1000, 640, false, false);
        assert_eq!((p.width, p.height), (800, 640));
        assert_eq!((p.x, p.y), (100, 0));
        let small = prepare(&blank(800, 640), 400, 400, false, false);
        assert_eq!((small.width, small.height), (400, 320));
        assert_eq!((small.x, small.y), (0, 40));
    }

    #[test]
    fn grey_edges_are_forced_to_black_or_white() {
        let mut image = blank(8, 1);
        image.put_pixel(7, 0, Luma([219])); // turned: becomes x = 0
        image.put_pixel(6, 0, Luma([220])); // turned: becomes x = 1
        let p = prepare(&image, 8, 1, false, false);
        assert!(!white_at(&p, 0, 0), "219 is ink");
        assert!(white_at(&p, 1, 0), "220 is paper");
    }

    #[test]
    fn rows_are_padded_to_four_bytes() {
        let p = prepare(&blank(33, 3), 33, 3, false, false);
        assert_eq!(p.bits.len(), 8 * 3);
    }

    #[test]
    fn the_thermal_switch_picks_the_printers_own_resolution() {
        assert_eq!(render_dpi(false, 600), 200.0);
        assert_eq!(render_dpi(true, 300), 300.0);
        assert_eq!(render_dpi(true, 0), 203.0);
        assert_eq!(render_dpi(true, 99_999), 203.0);
    }

    #[cfg(not(windows))]
    #[test]
    fn printing_off_windows_says_so() {
        let paper = crate::layout::Layout::default().paper;
        let job = Job {
            printer: "x",
            paper: &paper,
            thermal: false,
            rotate_90: false,
            document: "t",
            submission: None,
        };
        let system = SystemPrinting;
        assert_eq!(
            system.print(&job, &|_| blank(1, 1)),
            Err(PrintError::Unsupported)
        );
        assert_eq!(system.printers(), Err(PrintError::Unsupported));
    }
}
