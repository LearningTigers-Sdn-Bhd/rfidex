//! Per-PC badge settings and built-in printing. Construction never touches a driver.
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use rfidex_badge::layout::{Layout, Ticket};
use rfidex_badge::print::{Job, PrintError, Printing, Submission, SystemPrinting};
use rfidex_badge::settings::{ImportError, Settings};
use rfidex_badge::text::Fonts;
use serde::Serialize;

use crate::{AppPaths, RuntimeError};

#[derive(Clone)]
pub struct BadgeService {
    paths: AppPaths,
    printing: Arc<dyn Printing>,
    control: Arc<Mutex<Control>>,
}
struct Control {
    settings: Settings,
    warning: Option<String>,
    generation: u64,
}
#[derive(Clone)]
pub struct BadgeSelection {
    pub settings: Settings,
    generation: u64,
}
#[derive(Serialize)]
pub struct PrintersView {
    pub names: Vec<String>,
    pub default: Option<String>,
    pub supported: bool,
}
#[derive(Serialize)]
pub struct BadgeSettingsView {
    pub settings: Settings,
    pub printers: PrintersView,
    pub warning: Option<String>,
    pub provider: String,
}

fn with_fonts<R>(draw: impl FnOnce(&Fonts) -> R) -> R {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| Mutex::new(Fonts::new()));
    draw(&fonts.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Start the font system off the main path, so the first badge does not pay for
/// scanning every installed font. Only started when native printing is turned
/// On: with the switch Off no native code runs at startup.
pub fn warm_up() {
    std::thread::spawn(|| with_fonts(|_| ()));
}

fn disabled() -> RuntimeError {
    RuntimeError::new("native_print_disabled", "Built-in printing is not selected, or this job was cancelled before it was sent. RfiDex sent no badge. Press Reprint only if a badge is still needed.")
}
fn print_error(error: PrintError) -> RuntimeError {
    match error {
        PrintError::Cancelled => disabled(),
        PrintError::Unsupported => RuntimeError::new("print_unsupported", "Built-in printing works on Windows only. Use the event-printing app or the Windows desk PC."),
        PrintError::NoPrinter => RuntimeError::new("no_printer", "No printer is set up on this PC. Open Printer, pick one, then press Reprint."),
        PrintError::PrinterUnavailable => RuntimeError::new("printer_unavailable", "The printer cannot be reached. Check it is on and connected, then press Reprint."),
        PrintError::Rejected => RuntimeError::new("print_rejected", "The printer did not accept the badge. Check it for paper or errors, then press Reprint."),
    }
}
fn save_error() -> RuntimeError {
    RuntimeError::new("badge_settings_not_saved", "The printer settings could not be saved. The previous printing mode is still active. Check this PC's storage and try again.")
}

impl BadgeService {
    pub fn system(paths: AppPaths) -> Self {
        Self::with_printing(paths, Arc::new(SystemPrinting))
    }
    pub fn with_printing(paths: AppPaths, printing: Arc<dyn Printing>) -> Self {
        let (settings, warning) = match std::fs::read_to_string(paths.badge_file()) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(settings) => (settings.sanitized(), None),
                Err(_) => (Settings::default(), Some("The badge settings saved on this computer cannot be read. Badge printing stays on the event-printing app. Open Printer and press Save to replace them.".into())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
            Err(_) => (Settings::default(), Some("The badge settings saved on this computer cannot be read. Badge printing stays on the event-printing app. Open Printer and press Save to replace them.".into())),
        };
        Self {
            paths,
            printing,
            control: Arc::new(Mutex::new(Control {
                settings,
                warning,
                generation: 0,
            })),
        }
    }
    pub fn selection(&self) -> Option<BadgeSelection> {
        let state = self.control.lock().unwrap_or_else(|e| e.into_inner());
        state.settings.native_print_enabled.then(|| BadgeSelection {
            settings: state.settings.clone(),
            generation: state.generation,
        })
    }
    pub fn provider(&self) -> String {
        if self.selection().is_some() {
            "Badge printing: built-in"
        } else {
            "Badge printing: event-printing app"
        }
        .into()
    }
    pub fn view(&self) -> BadgeSettingsView {
        let (settings, mut warning) = {
            let state = self.control.lock().unwrap_or_else(|e| e.into_inner());
            (state.settings.clone(), state.warning.clone())
        };
        let printers = match self.printing.printers() {
            Ok(list) => PrintersView {
                names: list.names,
                default: list.default,
                supported: true,
            },
            Err(error) => {
                if error != PrintError::Unsupported {
                    warning.get_or_insert(print_error(error).message);
                }
                PrintersView {
                    names: vec![],
                    default: None,
                    supported: error != PrintError::Unsupported,
                }
            }
        };
        let provider = if settings.native_print_enabled {
            "Badge printing: built-in"
        } else {
            "Badge printing: event-printing app"
        }
        .into();
        BadgeSettingsView {
            settings,
            printers,
            warning,
            provider,
        }
    }
    fn persist(&self, settings: &Settings) -> Result<(), RuntimeError> {
        std::fs::create_dir_all(self.paths.root()).map_err(|_| save_error())?;
        let mut file =
            tempfile::NamedTempFile::new_in(self.paths.root()).map_err(|_| save_error())?;
        serde_json::to_writer_pretty(&mut file, settings).map_err(|_| save_error())?;
        file.write_all(b"\n").map_err(|_| save_error())?;
        file.as_file().sync_all().map_err(|_| save_error())?;
        file.persist(self.paths.badge_file())
            .map_err(|_| save_error())?;
        Ok(())
    }
    pub fn save(&self, settings: Settings) -> Result<(), RuntimeError> {
        let mut state = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = settings.sanitized();
        settings.native_print_enabled = state.settings.native_print_enabled;
        self.persist(&settings)?;
        state.settings = settings;
        state.warning = None;
        Ok(())
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<(), RuntimeError> {
        if enabled {
            let list = self.printing.printers().map_err(print_error)?;
            let chosen = self
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .settings
                .printer
                .trim()
                .to_lowercase();
            if chosen.is_empty() {
                if list.default.is_none() {
                    return Err(print_error(PrintError::NoPrinter));
                }
            } else if !list.names.iter().any(|n| n.to_lowercase() == chosen) {
                return Err(RuntimeError::new("printer_missing", "The selected printer is not installed on this PC. Choose another in Printer, press Save, then choose Built-in under Setup, Badge printing."));
            }
        }
        let mut state = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if enabled && state.settings.layout.clone().sanitize().is_none() {
            return Err(RuntimeError::new("bad_layout", "Choose a valid badge size and at least one field, then Save before choosing built-in printing."));
        }
        let mut settings = state.settings.clone();
        settings.native_print_enabled = enabled;
        self.persist(&settings)?;
        if enabled {
            warm_up();
        }
        if state.settings.native_print_enabled != enabled {
            state.generation = state.generation.wrapping_add(1);
        }
        state.settings = settings;
        state.warning = None;
        Ok(())
    }
    /// Invalidates work owned by a stopped runtime, preserving the saved switch.
    pub fn cancel_pending(&self) {
        let mut state = self.control.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
    }
    pub fn preview(&self, layout: Option<Layout>, ticket: &Ticket) -> Result<String, RuntimeError> {
        let layout = layout
            .unwrap_or_else(|| {
                self.control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .settings
                    .layout
                    .clone()
            })
            .sanitize()
            .ok_or_else(|| {
                RuntimeError::new(
                    "bad_layout",
                    "Choose a valid badge size and at least one field.",
                )
            })?;
        let image = with_fonts(|fonts| rfidex_badge::raster::render(&layout, ticket, fonts, 150.0));
        let png = rfidex_badge::raster::to_png(&image);
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ))
    }
    pub fn print_selected(
        &self,
        selection: &BadgeSelection,
        ticket: &Ticket,
        document: &str,
    ) -> Result<(), RuntimeError> {
        let guard = Permit {
            control: self.control.clone(),
            generation: selection.generation,
        };
        // Reject invalidated work before even opening a printer; check again at StartDoc.
        guard.check().map_err(print_error)?;
        let settings = &selection.settings;
        let job = Job {
            printer: &settings.printer,
            paper: &settings.layout.paper,
            thermal: settings.thermal,
            rotate_90: settings.rotate_90,
            document,
            submission: Some(&guard),
        };
        self.printing
            .print(&job, &|dpi| {
                with_fonts(|fonts| {
                    rfidex_badge::raster::render(&settings.layout, ticket, fonts, dpi)
                })
            })
            .map_err(print_error)
    }
    /// A test badge works with either printing method: it is how a printer is
    /// tried before any desk uses it, and it is not a desk job, so it needs no
    /// permit.
    pub fn test_print(&self) -> Result<(), RuntimeError> {
        let ticket: Ticket = serde_json::from_value(serde_json::json!({"ticket_id":"00000000-0000-0000-0000-000000000001","name":"Tan Wei Ming 陈伟明","company":"RfiDex test badge","ticket_type":"VIP","custom":{}})).expect("fixed sample badge");
        self.print_ticket(&ticket, "test")
    }
    /// Print one badge from details typed by staff, with the saved printer and
    /// layout. Like a test badge it works with either printing method and is
    /// never a desk job.
    pub fn print_ticket(&self, ticket: &Ticket, document: &str) -> Result<(), RuntimeError> {
        let settings = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settings
            .clone();
        let job = Job {
            printer: &settings.printer,
            paper: &settings.layout.paper,
            thermal: settings.thermal,
            rotate_90: settings.rotate_90,
            document,
            submission: None,
        };
        self.printing
            .print(&job, &|dpi| {
                with_fonts(|fonts| {
                    rfidex_badge::raster::render(&settings.layout, ticket, fonts, dpi)
                })
            })
            .map_err(print_error)
    }
    pub fn import_from(&self, path: Option<PathBuf>) -> Result<(), RuntimeError> {
        let path = path.or_else(import_path).ok_or_else(|| {
            RuntimeError::new(
                "import_missing",
                "No event-printing settings were found on this PC.",
            )
        })?;
        let json = std::fs::read_to_string(path).map_err(|_| {
            RuntimeError::new(
                "import_missing",
                "No readable event-printing settings were found on this PC.",
            )
        })?;
        let mut state = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let settings =
            Settings::from_event_printing(&json, state.settings.clone()).map_err(|error| {
                let message = match error {
                    ImportError::Unreadable => "The event-printing settings cannot be read.",
                    ImportError::NoLayout => {
                        "The event-printing settings do not contain a usable badge layout."
                    }
                };
                RuntimeError::new("import_empty", message)
            })?;
        self.persist(&settings)?;
        state.settings = settings;
        state.warning = None;
        Ok(())
    }
}

struct Permit {
    control: Arc<Mutex<Control>>,
    generation: u64,
}
impl Permit {
    fn check(&self) -> Result<(), PrintError> {
        let state = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if state.settings.native_print_enabled && state.generation == self.generation {
            Ok(())
        } else {
            Err(PrintError::Cancelled)
        }
    }
}
impl Submission for Permit {
    fn submit(&self, start: &mut dyn FnMut() -> Result<(), PrintError>) -> Result<(), PrintError> {
        // Checked, then the lock is released before the driver is called: a
        // driver can block in StartDoc (an offline network printer), and the
        // status bar and the Off switch both need this lock.
        self.check()?;
        start()
    }
}
fn import_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|root| PathBuf::from(root).join("event-printer/config.json"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|root| {
            PathBuf::from(root).join("Library/Application Support/event-printer/config.json")
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".config")))
            .map(|root| root.join("event-printer/config.json"))
    }
}
