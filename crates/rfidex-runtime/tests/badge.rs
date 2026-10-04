use rfidex_badge::{
    layout::{Layout, Ticket},
    print::{Job, PrintError, PrinterList, Printing},
    GrayImage,
};
use rfidex_runtime::{AppPaths, BadgeService, BadgeSettings};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recorder {
    jobs: Mutex<Vec<String>>,
}
impl Printing for Recorder {
    fn printers(&self) -> Result<PrinterList, PrintError> {
        Ok(PrinterList {
            names: vec!["Zebra".into()],
            default: Some("Zebra".into()),
        })
    }
    fn print(&self, job: &Job, render: &dyn Fn(f64) -> GrayImage) -> Result<(), PrintError> {
        let image = render(203.0);
        assert!(image.pixels().any(|p| p.0[0] < 220));
        let mut start = || {
            self.jobs.lock().unwrap().push(job.document.into());
            Ok(())
        };
        job.submit(&mut start)
    }
}
fn fixture() -> (tempfile::TempDir, BadgeService, Arc<Recorder>) {
    let temp = tempfile::tempdir().unwrap();
    let printing = Arc::new(Recorder::default());
    let service = BadgeService::with_printing(AppPaths::new(temp.path().into()), printing.clone());
    (temp, service, printing)
}
fn sample() -> Ticket {
    serde_json::from_value(serde_json::json!({"ticket_id":"sample","name":"Tan Wei Ming 陈伟明","company":"Expo","ticket_type":"VIP","custom":{}})).unwrap()
}

#[test]
fn fresh_old_and_corrupt_settings_are_off_without_driver_startup() {
    let (temp, service, _) = fixture();
    assert!(service.selection().is_none());
    assert!(!service.view().settings.native_print_enabled);
    for contents in ["{}", "broken"] {
        std::fs::write(temp.path().join("badge.json"), contents).unwrap();
        let loaded = BadgeService::system(AppPaths::new(temp.path().into()));
        assert!(loaded.selection().is_none());
        assert_eq!(loaded.view().warning.is_some(), contents == "broken");
    }
}
#[test]
fn save_import_and_restart_preserve_switch_and_printer() {
    let (temp, service, printing) = fixture();
    let settings = BadgeSettings {
        printer: " Zebra ".into(),
        ..Default::default()
    };
    service.save(settings).unwrap();
    service.set_enabled(true).unwrap();
    service.save(BadgeSettings::default()).unwrap();
    assert!(service.selection().is_some());
    let source = temp.path().join("old.json");
    std::fs::write(&source,r#"{"layout":{"paper":{"width_mm":104,"height_mm":155},"elements":["name","qr"]},"api_key":"DO-NOT-IMPORT"}"#).unwrap();
    service.import_from(Some(source)).unwrap();
    let saved = std::fs::read_to_string(temp.path().join("badge.json")).unwrap();
    assert!(!saved.contains("DO-NOT-IMPORT"));
    let restarted = BadgeService::with_printing(AppPaths::new(temp.path().into()), printing);
    assert!(restarted.selection().is_some());
    assert_eq!(restarted.view().settings.layout.paper.width_mm, 104.0);
    restarted.set_enabled(false).unwrap();
    assert!(restarted.selection().is_none());
}
#[test]
fn disabled_commands_and_old_generation_cannot_submit() {
    let (_, service, printing) = fixture();
    service.test_print().unwrap();
    assert!(
        service.selection().is_none(),
        "a test does not switch native on"
    );
    assert_eq!(printing.jobs.lock().unwrap().len(), 1);
    service.set_enabled(true).unwrap();
    let token = service.selection().unwrap();
    service.set_enabled(false).unwrap();
    service.set_enabled(true).unwrap();
    assert_eq!(
        service
            .print_selected(&token, &sample(), "old")
            .unwrap_err()
            .code,
        "native_print_disabled"
    );
    assert_eq!(
        printing.jobs.lock().unwrap().len(),
        1,
        "the old job was refused"
    );
    service.test_print().unwrap();
    assert_eq!(printing.jobs.lock().unwrap().len(), 2);
}
#[test]
fn a_manual_badge_prints_with_either_method_and_is_never_a_desk_job() {
    let (_, service, printing) = fixture();
    service.print_ticket(&sample(), "manual").unwrap();
    assert!(
        service.selection().is_none(),
        "printing by hand does not switch methods"
    );
    service.set_enabled(true).unwrap();
    service.print_ticket(&sample(), "manual").unwrap();
    assert_eq!(
        printing.jobs.lock().unwrap().as_slice(),
        ["manual", "manual"]
    );
}
#[test]
fn enabling_needs_the_chosen_printer_to_exist() {
    let (_, service, _) = fixture();
    let mut settings = BadgeSettings {
        printer: "Missing printer".into(),
        ..BadgeSettings::default()
    };
    service.save(settings.clone()).unwrap();
    assert_eq!(
        service.set_enabled(true).unwrap_err().code,
        "printer_missing"
    );
    assert!(service.selection().is_none());
    settings.printer = "zebra".into(); // the name is matched without case
    service.save(settings).unwrap();
    service.set_enabled(true).unwrap();
    assert!(service.selection().is_some());
}
#[test]
fn failed_persistence_keeps_previous_provider_and_settings() {
    let (temp, service, _) = fixture();
    std::fs::create_dir(temp.path().join("badge.json")).unwrap();
    assert!(service.set_enabled(true).is_err());
    assert!(service.selection().is_none());
    std::fs::remove_dir(temp.path().join("badge.json")).unwrap();
    service.set_enabled(true).unwrap();
    std::fs::remove_file(temp.path().join("badge.json")).unwrap();
    std::fs::create_dir(temp.path().join("badge.json")).unwrap();
    assert!(service.set_enabled(false).is_err());
    assert!(service.selection().is_some());
}
#[test]
fn preview_works_off_and_invalid_layout_is_refused() {
    let (_, service, printing) = fixture();
    let preview = service.preview(None, &sample()).unwrap();
    assert!(preview.starts_with("data:image/png;base64,iVBOR"));
    assert!(printing.jobs.lock().unwrap().is_empty());
    let mut bad = Layout::default();
    bad.paper.width_mm = 1.0;
    assert_eq!(
        service.preview(Some(bad), &sample()).unwrap_err().code,
        "bad_layout"
    );
}
#[cfg(not(windows))]
#[test]
fn unsupported_host_refuses_enable() {
    let temp = tempfile::tempdir().unwrap();
    let service = BadgeService::system(AppPaths::new(temp.path().into()));
    assert_eq!(
        service.set_enabled(true).unwrap_err().code,
        "print_unsupported"
    );
    assert!(service.selection().is_none());
}
