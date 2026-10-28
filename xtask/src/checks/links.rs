//! The `links` phase (PLAN.md, commit 41): a `mic://` link opened in alice's pod, as a desktop
//! opens one, reaches the window already running, and joins its room only once she says so: out
//! of a call with Join, in one by leaving it when the window asks. `mic --register` makes mic the
//! handler of the scheme; `xdg-open` starts a second mic, which hands the link to the first.

use std::time::Duration;

use mic_core::{Command, Invite};
use mic_harness::Snapshot;

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::TestBox;
use crate::view;

use super::{Checks, window};

/// How long the link may take to have joined.
const PATIENCE: Duration = Duration::from_secs(15);

/// How long a link has to arrive, out of a call, before it's taken not to have joined by itself.
const SETTLE: Duration = Duration::from_secs(3);

pub fn links(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let mut alice = checks.require("alice answers", || Participant::connect(the_box, "alice"))?;
    checks.require("mic registers as what opens mic:// links", || {
        run(the_box, "alice", &["/opt/mic/mic", "--register"])
    })?;
    let open = |invite: &Invite| {
        // On alice's display, as a desktop's xdg-open runs: its generic mode looks for a
        // scheme's handler only with a $DISPLAY.
        run(
            the_box,
            "alice",
            &["env", "DISPLAY=:1", "xdg-open", &invite.to_mic_url()],
        )
    };
    checks.record(
        "a link opened out of a call is filled in, and joins with Enter, under her name",
        || {
            // A name to join under: the one she last joined with.
            alice.join(&Invite::new_room(), "Alice")?;
            alice.reset()?;
            let invite = Invite::new_room();
            open(&invite)?;
            // Long enough for the link to have arrived, and to have joined if it did by itself.
            std::thread::sleep(SETTLE);
            let snapshot = alice.snapshot()?;
            if snapshot.room.is_some() {
                return Err(Error::Failed(
                    "alice joined the link's room without a click".to_owned(),
                ));
            }
            window::press(the_box, "alice", "Return")?;
            alice.wait_for("in the link's room", PATIENCE, |state| {
                state.room == invite.room.to_string() && state.me.profile.name == "Alice"
            })?;
            Ok(())
        },
    );
    checks.record(
        "a link opened in a call asks first, naming its room: Escape stays, Enter leaves for it, still muted",
        || {
            let here = Invite::new_room();
            alice.join(&here, "Alice")?;
            alice.act(Command::SetMuted { muted: true })?;
            // A named room: the window asks about it by name, and shows it once in it.
            let there = Invite::new_room().named("Standup");
            let link = there.to_mic_url();
            let asks = |snapshot: &Snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| {
                    ui.asking.as_deref() == Some(link.as_str())
                        && ui.asking_name.as_deref() == Some("Standup")
                })
            };
            open(&there)?;
            alice.wait_for_snapshot("asking about the link", PATIENCE, asks)?;
            window::press(the_box, "alice", "Escape")?;
            alice.wait_for_snapshot("staying, asking no more", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| ui.asking.is_none())
                    && snapshot
                        .room
                        .as_ref()
                        .is_some_and(|room| room.room == here.room.to_string())
            })?;
            open(&there)?;
            alice.wait_for_snapshot("asking about the link again", PATIENCE, asks)?;
            window::press(the_box, "alice", "Return")?;
            alice.wait_for("in the link's room, still muted", PATIENCE, |state| {
                state.room == there.room.to_string()
                    && state.me.profile.name == "Alice"
                    && state.me.profile.muted
                    && state.name.as_deref() == Some("Standup")
            })?;
            alice.wait_for_snapshot("its name shown", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().and_then(|ui| ui.room_name.as_deref()) == Some("Standup")
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
