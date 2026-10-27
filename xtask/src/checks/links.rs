//! The `links` phase (PLAN.md, commit 41): a `mic://` link opened in alice's pod, as a desktop
//! opens one, joins her to its room in the window already running. `mic --register` makes mic the
//! handler of the scheme; `xdg-open` starts a second mic, which hands the link to the first.

use std::time::Duration;

use mic_core::Invite;

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::TestBox;
use crate::view;

use super::Checks;

/// How long the link may take to have joined.
const PATIENCE: Duration = Duration::from_secs(15);

pub fn links(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let mut alice = checks.require("alice answers", || Participant::connect(the_box, "alice"))?;
    checks.require("mic registers as what opens mic:// links", || {
        run(the_box, "alice", &["/opt/mic/mic", "--register"])
    })?;
    checks.record(
        "a link opened joins alice to its room, under her name",
        || {
            // A name to join under: the one she last joined with.
            alice.join(&Invite::new_room(), "Alice")?;
            alice.reset()?;
            let invite = Invite::new_room();
            // On alice's display, as a desktop's xdg-open runs: its generic mode looks for a scheme's
            // handler only with a $DISPLAY.
            run(
                the_box,
                "alice",
                &["env", "DISPLAY=:1", "xdg-open", &invite.to_mic_url()],
            )?;
            alice.wait_for("in the link's room", PATIENCE, |state| {
                state.room == invite.room.to_string() && state.me.profile.name == "Alice"
            })?;
            Ok(())
        },
    );
    alice.reset()?;
    Ok(())
}

/// Runs `command` in `client`'s pod, as its user.
fn run(the_box: &TestBox, client: &'static str, command: &[&str]) -> Result<()> {
    let ran = the_box.exec(&view::container(client), command)?;
    if ran.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{command:?} in {client}: {}{}",
            String::from_utf8_lossy(&ran.stdout).trim(),
            String::from_utf8_lossy(&ran.stderr).trim()
        )))
    }
}
