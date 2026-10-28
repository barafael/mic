//! The `version` phase (D15): the box's site announces a newer version than any mic
//! (`podman::NEWER`), and every client's window says so, from the version check it made at start;
//! Escape dismisses it, as "Not now" does, until the window starts again.

use std::time::Duration;

use crate::error::Result;
use crate::harness::Participant;
use crate::podman::{CLIENTS, NEWER, Pod, TestBox};
use crate::{process, ready};

use super::Checks;

/// How long a client may take to have checked, after it started.
const PATIENCE: Duration = Duration::from_secs(15);

pub fn version(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    for client in CLIENTS {
        checks.record(&format!("{client}'s window says {NEWER} is out"), || {
            let mut participant = Participant::connect(the_box, client)?;
            participant.wait_for_snapshot(
                "saying a newer version is out",
                PATIENCE,
                |snapshot| {
                    snapshot
                        .ui
                        .as_ref()
                        .and_then(|ui| ui.notice.as_deref())
                        .is_some_and(|notice| notice.contains(NEWER))
                },
            )?;
            Ok(())
        });
    }
    checks.record(
        "Escape dismisses it, and a restarted window says it again",
        || {
            let mut frank = Participant::connect(the_box, "frank")?;
            frank.reset()?;
            super::window::press(the_box, "frank", "Escape")?;
            frank.wait_for_snapshot("the notice dismissed", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| ui.notice.is_none())
            })?;
            // Dismissed for this run of the window only: the next start says it again, as the
            // phase's next round wants.
            process::run(crate::podman::podman(&[
                "restart",
                "--time",
                "10",
                &Pod::Client("frank").container("client"),
            ]))?;
            ready::client_up(the_box, "frank")?;
            Participant::connect(the_box, "frank")?.wait_for_snapshot(
                "saying it again",
                PATIENCE,
                |snapshot| {
                    snapshot
                        .ui
                        .as_ref()
                        .and_then(|ui| ui.notice.as_deref())
                        .is_some_and(|notice| notice.contains(NEWER))
                },
            )?;
            Ok(())
        },
    );
    Ok(())
}
