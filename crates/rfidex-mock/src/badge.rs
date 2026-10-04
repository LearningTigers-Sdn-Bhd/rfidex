//! Native printer recorder with deterministic holds before and after submission.
use rfidex_badge::{
    print::{Job, PrintError, PrinterList, Printing},
    GrayImage,
};
use std::sync::{Condvar, Mutex};

#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Success,
    Fail(PrintError),
    HeldBeforeSubmit,
    HeldAfterSubmit,
}
#[derive(Clone, Debug)]
pub struct Printed {
    pub printer: String,
    pub document: String,
    pub thermal: bool,
    pub paper_tenths_mm: (i32, i32),
    pub pixels: (u32, u32),
    pub dark_pixels: usize,
}
#[derive(Default)]
struct State {
    mode: Mode,
    requested: usize,
    released: bool,
    jobs: Vec<Printed>,
}
#[derive(Default)]
pub struct FakePrinting {
    state: Mutex<State>,
    wake: Condvar,
    notified: tokio::sync::Notify,
}
impl FakePrinting {
    pub fn set_mode(&self, mode: Mode) {
        let mut state = self.state.lock().unwrap();
        state.mode = mode;
        state.released = false;
        state.requested = 0;
    }
    pub fn jobs(&self) -> Vec<Printed> {
        self.state.lock().unwrap().jobs.clone()
    }
    pub fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.wake.notify_all();
    }
    pub async fn wait_for_request(&self) {
        loop {
            let notified = self.notified.notified();
            if self.state.lock().unwrap().requested > 0 {
                return;
            }
            notified.await;
        }
    }
    fn hold(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.released {
            state = self.wake.wait(state).unwrap();
        }
    }
    fn requested(&self) {
        self.state.lock().unwrap().requested += 1;
        self.notified.notify_one();
    }
}
impl Printing for FakePrinting {
    fn printers(&self) -> Result<PrinterList, PrintError> {
        Ok(PrinterList {
            names: vec!["Zebra".into()],
            default: Some("Zebra".into()),
        })
    }
    fn print(&self, job: &Job, render: &dyn Fn(f64) -> GrayImage) -> Result<(), PrintError> {
        let mode = self.state.lock().unwrap().mode;
        if let Mode::Fail(error) = mode {
            return Err(error);
        }
        let image = render(203.0);
        if matches!(mode, Mode::HeldBeforeSubmit) {
            self.requested();
            self.hold();
        }
        job.submit(&mut || {
            self.state.lock().unwrap().jobs.push(Printed {
                printer: job.printer.into(),
                document: job.document.into(),
                thermal: job.thermal,
                paper_tenths_mm: (
                    (job.paper.width_mm * 10.0).round() as i32,
                    (job.paper.height_mm * 10.0).round() as i32,
                ),
                pixels: image.dimensions(),
                dark_pixels: image.pixels().filter(|p| p.0[0] < 220).count(),
            });
            Ok(())
        })?;
        if matches!(mode, Mode::HeldAfterSubmit) {
            self.requested();
            self.hold();
        }
        Ok(())
    }
}
