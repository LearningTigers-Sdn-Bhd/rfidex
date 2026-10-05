//! What this PC remembers about badges: the layout, the saved layouts, the
//! printer, and the thick-stroke switch. One small JSON file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::layout::Layout;

pub const MAX_PRESETS: usize = 20;
const MAX_PRESET_NAME: usize = 40;
const MAX_PRINTER_NAME: usize = 200;
const MAX_BADGE_TYPES: usize = 30;
const MAX_BADGE_TYPE_NAME: usize = 40;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Native printing is opt-in, including settings from older releases.
    pub native_print_enabled: bool,
    pub layout: Layout,
    /// Saved layouts by name.
    pub presets: BTreeMap<String, Layout>,
    pub active_preset: Option<String>,
    /// Thicker strokes, for direct-thermal stock that drops dots.
    pub thermal: bool,
    /// Rotate the badge for stock loaded across the shorter edge.
    pub rotate_90: bool,
    /// The Windows printer's name. Empty means the Windows default printer.
    pub printer: String,
    /// Ticket types added by hand, offered in Manual print next to the event's own.
    pub badge_types: Vec<String>,
}

/// Trimmed, no blanks, no repeats (ignoring case), at most `MAX_BADGE_TYPES`.
fn clean_types(list: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in list {
        let name: String = item.trim().chars().take(MAX_BADGE_TYPE_NAME).collect();
        if name.is_empty() || out.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
            continue;
        }
        out.push(name);
        if out.len() >= MAX_BADGE_TYPES {
            break;
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// Not JSON, or not a settings file.
    Unreadable,
    /// event-printing had never saved a badge layout.
    NoLayout,
}

impl Settings {
    /// The same settings, made safe to save and print: an unusable layout is
    /// replaced by the default, bad presets are dropped, names are trimmed.
    pub fn sanitized(self) -> Settings {
        let layout = self.layout.sanitize().unwrap_or_default();
        let mut presets = BTreeMap::new();
        for (name, preset) in self.presets {
            let name: String = name.trim().chars().take(MAX_PRESET_NAME).collect();
            if name.is_empty() || presets.len() >= MAX_PRESETS {
                continue;
            }
            if let Some(preset) = preset.sanitize() {
                presets.insert(name, preset);
            }
        }
        let active_preset = self.active_preset.filter(|name| presets.contains_key(name));
        Settings {
            native_print_enabled: self.native_print_enabled,
            layout,
            presets,
            active_preset,
            thermal: self.thermal,
            rotate_90: self.rotate_90,
            printer: self.printer.trim().chars().take(MAX_PRINTER_NAME).collect(),
            badge_types: clean_types(self.badge_types),
        }
    }

    /// Take the layouts and thick-stroke switch from event-printing's own
    /// `config.json`, keeping this PC's printer choice. Its connection details
    /// (address, event name, API key) are never read.
    pub fn from_event_printing(json: &str, keep: Settings) -> Result<Settings, ImportError> {
        let root: Value = serde_json::from_str(json).map_err(|_| ImportError::Unreadable)?;
        let object = root.as_object().ok_or(ImportError::Unreadable)?;
        let layout: Layout = object
            .get("layout")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .ok_or(ImportError::NoLayout)?;
        let presets: BTreeMap<String, Layout> = object
            .get("layout_presets")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(name, v)| {
                        Some((name.clone(), serde_json::from_value(v.clone()).ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let imported = Settings {
            native_print_enabled: keep.native_print_enabled,
            layout,
            presets,
            active_preset: object
                .get("active_preset")
                .and_then(Value::as_str)
                .map(String::from),
            thermal: object
                .get("direct_thermal")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rotate_90: keep.rotate_90,
            printer: keep.printer,
            // event-printing's own Badge types come across, added to this PC's.
            badge_types: keep
                .badge_types
                .into_iter()
                .chain(
                    object
                        .get("badge_types")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(String::from),
                )
                .collect(),
        }
        .sanitized();
        // A layout that did not survive cleaning is not an import.
        if imported.layout == Layout::default() && !layout_was_default(object) {
            return Err(ImportError::NoLayout);
        }
        Ok(imported)
    }
}

/// True when the file's own layout really was the default one, so cleaning
/// it to the default is not a failure.
fn layout_was_default(object: &serde_json::Map<String, Value>) -> bool {
    object
        .get("layout")
        .and_then(|v| serde_json::from_value::<Layout>(v.clone()).ok())
        .and_then(Layout::sanitize)
        .is_some_and(|l| l == Layout::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT_PRINTING: &str = r#"{
      "backend_url": "https://x.example", "event_slug": "demo", "api_key": "secret-key-never-read",
      "badge_types": ["Visitor"], "show_scanner": true, "direct_thermal": true,
      "layout": {"paper": {"width_mm": 104, "height_mm": 155},
                 "elements": ["name", "role", "qr"], "vertical_offset_mm": 4.0},
      "layout_presets": {
        "Card": {"paper": {"width_mm": 104, "height_mm": 155}, "elements": ["name", "qr"]},
        "Broken": {"paper": {"width_mm": 1, "height_mm": 1}, "elements": ["name"]}
      },
      "active_preset": "Card"
    }"#;

    #[test]
    fn event_printing_layouts_come_across_and_credentials_do_not() {
        let keep = Settings {
            printer: "Zebra ZD421".into(),
            rotate_90: true,
            ..Settings::default()
        };
        let imported = Settings::from_event_printing(EVENT_PRINTING, keep).unwrap();
        assert_eq!(imported.layout.paper.width_mm, 104.0);
        assert_eq!(imported.layout.vertical_offset_mm, 4.0);
        assert!(imported.thermal);
        assert!(imported.rotate_90);
        assert_eq!(imported.printer, "Zebra ZD421");
        assert_eq!(imported.presets.len(), 1, "the unusable preset is dropped");
        assert_eq!(imported.active_preset.as_deref(), Some("Card"));
        let saved = serde_json::to_string(&imported).unwrap();
        assert!(!saved.contains("secret-key-never-read"));
        assert!(!saved.contains("x.example"));
    }

    #[test]
    fn a_file_without_a_layout_or_not_json_is_refused() {
        assert_eq!(
            Settings::from_event_printing(r#"{"backend_url": "x"}"#, Settings::default()),
            Err(ImportError::NoLayout)
        );
        assert_eq!(
            Settings::from_event_printing("nope", Settings::default()),
            Err(ImportError::Unreadable)
        );
        assert_eq!(
            Settings::from_event_printing(
                r#"{"layout": {"paper": {"width_mm": 1, "height_mm": 1}, "elements": ["name"]}}"#,
                Settings::default()
            ),
            Err(ImportError::NoLayout)
        );
    }

    #[test]
    fn sanitizing_repairs_what_a_save_could_carry() {
        let mut settings = Settings::default();
        settings.layout.paper.width_mm = 1.0; // unusable: back to the default
        settings.presets.insert("  ".into(), Layout::default());
        settings.presets.insert(" Good ".into(), Layout::default());
        settings.active_preset = Some("Missing".into());
        settings.printer = "  Zebra  ".into();
        let clean = settings.sanitized();
        assert_eq!(clean.layout, Layout::default());
        assert_eq!(clean.presets.keys().collect::<Vec<_>>(), vec!["Good"]);
        assert_eq!(clean.active_preset, None);
        assert_eq!(clean.printer, "Zebra");
    }

    #[test]
    fn importing_keeps_native_printing_off_or_on() {
        for enabled in [false, true] {
            let mut json = serde_json::to_value(Settings::default()).unwrap();
            json["native_print_enabled"] = enabled.into();
            let keep = serde_json::from_value(json).unwrap();
            let imported = Settings::from_event_printing(EVENT_PRINTING, keep).unwrap();
            assert_eq!(
                serde_json::to_value(imported).unwrap()["native_print_enabled"],
                enabled
            );
        }
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            serde_json::to_value(old).unwrap()["native_print_enabled"],
            false
        );
    }

    #[test]
    fn badge_types_are_cleaned_and_event_printing_types_are_added_not_replaced() {
        let settings = Settings {
            badge_types: vec![
                " Sponsor ".into(),
                "sponsor".into(),
                "".into(),
                "Press".into(),
            ],
            ..Settings::default()
        };
        let clean = settings.sanitized();
        assert_eq!(clean.badge_types, vec!["Sponsor", "Press"]);
        let imported = Settings::from_event_printing(EVENT_PRINTING, clean).unwrap();
        assert_eq!(imported.badge_types, vec!["Sponsor", "Press", "Visitor"]);
        let many = Settings {
            badge_types: (0..60).map(|n| format!("Type {n}")).collect(),
            ..Settings::default()
        };
        assert_eq!(many.sanitized().badge_types.len(), 30);
    }

    #[test]
    fn an_old_or_empty_file_loads_with_defaults() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, Settings::default());
    }
}
