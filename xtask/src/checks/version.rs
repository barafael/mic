//! The `version` phase (D15): the box's site announces a newer version than any mic
//! (`podman::NEWER`), and every client's window says so, from the version check it made at start.

use std::time::Duration;

use crate::error::Result;
use crate::harness::Participant;
use crate::podman::{CLIENTS, NEWER, TestBox};

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
    Ok(())
}
