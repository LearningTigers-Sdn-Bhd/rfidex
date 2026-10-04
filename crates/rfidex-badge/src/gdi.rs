//! Windows GDI printing: open the printer, ask for the badge's paper size,
//! and send one bitmap. The same calls event-printing made through pywin32.

use std::ffi::c_void;

use image::GrayImage;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetDeviceCaps, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DEVMODEW, DIB_RGB_COLORS, DM_IN_BUFFER, DM_OUT_BUFFER, DM_PAPERLENGTH, DM_PAPERSIZE,
    DM_PAPERWIDTH, HDC, HORZRES, LOGPIXELSX, RGBQUAD, SRCCOPY, VERTRES,
};
use windows::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW,
    PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
};
use windows::Win32::Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

use crate::layout::Paper;
use crate::print::{prepare, render_dpi, Job, PrintError, PrinterList};

const IDOK: i32 = 1;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A zeroed buffer the system can write structs into: `u64` backing keeps the
/// 8-byte alignment `PRINTER_INFO_4W` and `DEVMODEW` need.
fn aligned(bytes: usize) -> Vec<u64> {
    vec![0u64; bytes.div_ceil(8)]
}

pub fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // The first call only reports how long the name is.
    unsafe {
        let _ = GetDefaultPrinterW(None, &mut len);
    }
    if len == 0 {
        return None;
    }
    let mut buffer = vec![0u16; len as usize];
    let ok = unsafe { GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut len) };
    if !ok.as_bool() {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

pub fn printers() -> Result<PrinterList, PrintError> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut returned) = (0u32, 0u32);
    // The first call only reports how much room the list needs.
    unsafe {
        let _ = EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut returned);
    }
    let mut names = Vec::new();
    if needed > 0 {
        let mut backing = aligned(needed as usize);
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(backing.as_mut_ptr() as *mut u8, needed as usize)
        };
        unsafe {
            EnumPrintersW(
                flags,
                PCWSTR::null(),
                4,
                Some(bytes),
                &mut needed,
                &mut returned,
            )
        }
        .map_err(|_| PrintError::NoPrinter)?;
        let infos = backing.as_ptr() as *const PRINTER_INFO_4W;
        for i in 0..returned as usize {
            let info = unsafe { &*infos.add(i) };
            if let Ok(name) = unsafe { info.pPrinterName.to_string() } {
                names.push(name);
            }
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    Ok(PrinterList {
        names,
        default: default_printer(),
    })
}

pub fn print(job: &Job, render: &dyn Fn(f64) -> GrayImage) -> Result<(), PrintError> {
    let name = if job.printer.trim().is_empty() {
        default_printer().ok_or(PrintError::NoPrinter)?
    } else {
        job.printer.to_string()
    };
    let name_w = wide(&name);
    let mut handle = PRINTER_HANDLE::default();
    unsafe { OpenPrinterW(PCWSTR(name_w.as_ptr()), &mut handle, None) }
        .map_err(|_| PrintError::PrinterUnavailable)?;
    let outcome = unsafe { print_open(handle, &name_w, job, render) };
    unsafe {
        let _ = ClosePrinter(handle);
    }
    outcome
}

unsafe fn print_open(
    handle: PRINTER_HANDLE,
    name_w: &[u16],
    job: &Job,
    render: &dyn Fn(f64) -> GrayImage,
) -> Result<(), PrintError> {
    let hdc = create_dc(handle, name_w, job.paper)?;
    let outcome = send_page(hdc, job, render);
    let _ = DeleteDC(hdc);
    outcome
}

/// A device context for the printer with the badge's paper size asked for.
///
/// ponytail: some label-printer drivers only accept sizes from their own list
/// and ignore a custom size. Then the printer's own default page is used,
/// exactly as event-printing did.
unsafe fn create_dc(
    handle: PRINTER_HANDLE,
    name_w: &[u16],
    paper: &Paper,
) -> Result<HDC, PrintError> {
    let name = PCWSTR(name_w.as_ptr());
    let size = DocumentPropertiesW(None, handle, name, None, None, 0);
    if size > 0 {
        let mut backing = aligned(size as usize);
        let devmode = backing.as_mut_ptr() as *mut DEVMODEW;
        if DocumentPropertiesW(None, handle, name, Some(devmode), None, DM_OUT_BUFFER.0) == IDOK {
            let sizes = &mut (*devmode).Anonymous1.Anonymous1;
            sizes.dmPaperSize = 0; // 0 = use the width and length below
            sizes.dmPaperWidth = (paper.width_mm * 10.0).round() as i16; // tenths of a mm
            sizes.dmPaperLength = (paper.height_mm * 10.0).round() as i16;
            (*devmode).dmFields =
                (*devmode).dmFields | DM_PAPERSIZE | DM_PAPERWIDTH | DM_PAPERLENGTH;
            let merged = DocumentPropertiesW(
                None,
                handle,
                name,
                Some(devmode),
                Some(devmode as *const DEVMODEW),
                (DM_IN_BUFFER | DM_OUT_BUFFER).0,
            );
            if merged == IDOK {
                let hdc = CreateDCW(
                    w!("WINSPOOL"),
                    name,
                    PCWSTR::null(),
                    Some(devmode as *const DEVMODEW),
                );
                if !hdc.is_invalid() {
                    return Ok(hdc);
                }
            }
        }
    }
    let hdc = CreateDCW(w!("WINSPOOL"), name, PCWSTR::null(), None);
    if hdc.is_invalid() {
        Err(PrintError::PrinterUnavailable)
    } else {
        Ok(hdc)
    }
}

/// `BITMAPINFO` holds one colour; a 1-bit picture needs two.
#[repr(C)]
struct Bitmap1Info {
    header: BITMAPINFOHEADER,
    colors: [RGBQUAD; 2],
}

unsafe fn send_page(
    hdc: HDC,
    job: &Job,
    render: &dyn Fn(f64) -> GrayImage,
) -> Result<(), PrintError> {
    let printable_w = GetDeviceCaps(Some(hdc), HORZRES).max(1) as u32;
    let printable_h = GetDeviceCaps(Some(hdc), VERTRES).max(1) as u32;
    let dpi = render_dpi(job.thermal, GetDeviceCaps(Some(hdc), LOGPIXELSX));
    let page = prepare(&render(dpi), printable_w, printable_h, job.thermal);

    let document = wide(&format!("RfiDex badge {}", job.document));
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(document.as_ptr()),
        lpszOutput: PCWSTR::null(),
        lpszDatatype: PCWSTR::null(),
        fwType: 0,
    };
    job.submit(&mut || {
        if StartDocW(hdc, &info) <= 0 {
            Err(PrintError::Rejected)
        } else {
            Ok(())
        }
    })?;
    if StartPage(hdc) <= 0 {
        AbortDoc(hdc);
        return Err(PrintError::Rejected);
    }
    let bitmap = Bitmap1Info {
        header: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: page.width as i32,
            // Positive: a bottom-up DIB, the form every printer driver accepts.
            // Some drivers mishandle a top-down (negative height) DIB.
            biHeight: page.height as i32,
            biPlanes: 1,
            biBitCount: 1,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        // Index 0 is a clear bit (ink), index 1 a set bit (paper).
        colors: [
            RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            },
            RGBQUAD {
                rgbBlue: 255,
                rgbGreen: 255,
                rgbRed: 255,
                rgbReserved: 0,
            },
        ],
    };
    // `prepare` keeps the top row first; a bottom-up DIB wants the last row first.
    let stride = page.bits.len() / page.height as usize;
    let mut bottom_up = Vec::with_capacity(page.bits.len());
    for row in page.bits.chunks(stride).rev() {
        bottom_up.extend_from_slice(row);
    }
    let lines = StretchDIBits(
        hdc,
        page.x,
        page.y,
        page.width as i32,
        page.height as i32,
        0,
        0,
        page.width as i32,
        page.height as i32,
        Some(bottom_up.as_ptr() as *const c_void),
        &bitmap as *const Bitmap1Info as *const BITMAPINFO,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
    if lines <= 0 {
        AbortDoc(hdc);
        return Err(PrintError::Rejected);
    }
    if EndPage(hdc) <= 0 {
        AbortDoc(hdc);
        return Err(PrintError::Rejected);
    }
    if EndDoc(hdc) <= 0 {
        return Err(PrintError::Rejected);
    }
    Ok(())
}
