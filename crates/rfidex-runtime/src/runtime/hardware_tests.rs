//! The operator's reader tests: hardware probe and the sticker write test.

use uuid::Uuid;

use super::device::StationDevice;
use super::probe::probe_station;
use super::Runtime;
use crate::config::{AppConfig, DeviceChoice, StationConfig};
use crate::hardware::{self, HardwareTestAction, HardwareTestView};
use crate::sticker_test::{self, StickerTestStep, StickerTestView};
use crate::RuntimeError;

impl Runtime {
    /// Run one of the operator's reader tests on a saved station.
    ///
    /// It goes through the station's own adapter, so it cannot open a second
    /// handle to a reader that is already in use. The work is synchronous and
    /// can wait on a reader, so it runs on a blocking worker like every other
    /// hardware call.
    pub async fn hardware_test(
        &self,
        station: Uuid,
        action: HardwareTestAction,
    ) -> Result<HardwareTestView, RuntimeError> {
        let runtime = self.station(station)?.clone();
        let handle = tokio::runtime::Handle::current();
        self.hardware_jobs
            .run(move || handle.block_on(async move { probe_station(&runtime, action).await }))
            .await
            .map_err(|_| hardware::no_helper())
    }

    /// One step of the disposable-sticker write test on a real SDK desk.
    ///
    /// The desk's own session is held for the whole step and its reader is let
    /// go, so the one-off test helper is the only thing talking to the
    /// reader. The desk reconnects afterwards with writing still exactly as its
    /// saved profile says.
    // ponytail: the test helper is not registered with shutdown's stop control;
    // a step caught by shutdown finishes within the reader's own timeout per call.
    pub async fn sticker_test(
        &self,
        station: Uuid,
        step: StickerTestStep,
    ) -> Result<StickerTestView, RuntimeError> {
        let runtime = self.station(station)?.clone();
        let hardware = sdk_desk_hardware(&runtime.config)?;
        let write_enabled = hardware.write_verified();
        let root = self.paths.root().to_path_buf();
        let pending = self
            .tear
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|p| p.station == station);

        let outcome = match step {
            StickerTestStep::Status => sticker_test::status_outcome(),
            StickerTestStep::TearCheck if pending.is_none() => {
                sticker_test::refused("Press Start tear first, then put the sticker back.")
            }
            _ => {
                let launcher = self.launcher.clone();
                let start_block = runtime.config.write_start_block;
                let desk = runtime.clone();
                let waiting = pending.clone();
                let handle = tokio::runtime::Handle::current();
                let (outcome, next) = self
                    .hardware_jobs
                    .run(move || {
                        handle.block_on(async move {
                            let StationDevice::Desk(device) = &desk.device else {
                                return (sticker_test::refused(sticker_test::NOT_A_SDK_DESK), None);
                            };
                            let mut session = device.lock().await;
                            session.station.reader.release();
                            // The desktop helper refuses commissioning mode, so
                            // the test runs an ordinary helper whose own copy of
                            // the profile allows writes. The desk's session keeps
                            // the saved profile, so it still cannot write.
                            let mut test_profile = hardware.clone();
                            test_profile.set_write_verified(true);
                            let result = match rfidex_hardware::HardwareClient::start(
                                &launcher,
                                &test_profile,
                            ) {
                                Err(_) => (sticker_test::refused(TEST_READER_BUSY), None),
                                Ok(mut client) => {
                                    let result = match (step, waiting) {
                                        (StickerTestStep::TearWrite, _) => {
                                            sticker_test::tear_write(
                                                &mut client,
                                                station,
                                                start_block,
                                            )
                                        }
                                        (StickerTestStep::TearCheck, Some(p)) => {
                                            (sticker_test::tear_check(&mut client, &p), None)
                                        }
                                        _ => (
                                            sticker_test::check(&mut client, station, start_block),
                                            None,
                                        ),
                                    };
                                    if client.is_alive() {
                                        let _ = client.call(rfidex_hardware::Operation::Close);
                                    }
                                    result
                                }
                            };
                            // Give the desk its reader back now rather than on the
                            // next scan, so the status bar stays truthful.
                            let _ = session.station.reader.probe();
                            result
                        })
                    })
                    .await?;
                runtime.refresh_desk_connection().await;
                let mut tear = self.tear.lock().unwrap_or_else(|e| e.into_inner());
                match step {
                    StickerTestStep::TearWrite if next.is_some() => *tear = next,
                    // A wrong sticker leaves the trial open; anything else ends it.
                    StickerTestStep::TearCheck if outcome.ok || outcome.record.is_some() => {
                        *tear = None
                    }
                    _ => {}
                }
                outcome
            }
        };
        if let Some(record) = &outcome.record {
            sticker_test::append(&root, record).map_err(|_| {
                RuntimeError::new(
                    "save_failed",
                    "The test ran, but its result could not be saved. Check the disk and run it again.",
                )
            })?;
        }
        let tear_waiting = self
            .tear
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|p| p.station == station);
        Ok(sticker_test::view(
            outcome,
            &sticker_test::load(&root, station),
            tear_waiting,
            write_enabled,
        ))
    }

    /// The saved setup with writing turned on or off for one desk. Turning it
    /// on needs one passed sticker and no failure; turning it off always works.
    /// The caller saves it and restarts the stations, as Setup does.
    pub fn with_writing(&self, station: Uuid, on: bool) -> Result<AppConfig, RuntimeError> {
        sdk_desk_hardware(&self.station(station)?.config)?;
        if on {
            let tally = sticker_test::tally(&sticker_test::load(self.paths.root(), station));
            if !sticker_test::can_enable(&tally) {
                return Err(RuntimeError::new(
                    "test_not_passed",
                    "Writing can be turned on after at least one sticker passes the test with no failures.",
                ));
            }
        }
        let mut config = self.config();
        for s in config.stations.iter_mut().filter(|s| s.id == station) {
            if let DeviceChoice::EcrfidDesk { hardware } = &mut s.device {
                hardware.set_write_verified(on);
            }
        }
        Ok(config)
    }
}

const TEST_READER_BUSY: &str = "The test could not open the reader. Check that it is connected and no other program is using it.";

fn sdk_desk_hardware(
    station: &StationConfig,
) -> Result<rfidex_hardware::HardwareConfig, RuntimeError> {
    match &station.device {
        DeviceChoice::EcrfidDesk { hardware } if hardware.is_sdk() => Ok(hardware.clone()),
        _ => Err(RuntimeError::new(
            "not_sdk_desk",
            sticker_test::NOT_A_SDK_DESK,
        )),
    }
}
