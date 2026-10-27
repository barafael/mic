//! The `roster` phase: alice and bob join a room of their own and see each other, in their
//! windows too, a new name reaches the other, bob leaves and alice sees him go, a restarted relay
//! is rejoined, and a restarted client is the same participant, its device secret kept in its
//! keyring; all through their harnesses, with screenshots of their windows.

use std::path::Path;
use std::time::Duration;

use mic_core::{Command, Connection, Invite, RoomState};
use mic_harness::Snapshot;

use crate::error::{Error, Result};
use crate::harness::{Participant, names};
use crate::podman::{Pod, TestBox};
use crate::{process, ready, view};

use super::Checks;

/// How long a roster may take to show a change.
const PATIENCE: Duration = Duration::from_secs(15);

pub fn roster(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut bob) = checks.require("alice and bob answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
        ))
    })?;
    // A room of its own: no former run's or concurrent phase's participants in it.
    let invite = Invite::new_room();
    checks.require("alice and bob see each other", || {
        alice.join(&invite, "Alice")?;
        bob.join(&invite, "Bob")?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        bob.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        Ok(())
    })?;
    let directory = checks.record_directory().to_owned();
    checks.record("their windows show the room's tiles", || {
        for participant in [&mut alice, &mut bob] {
            participant.wait_for_snapshot("showing the room", PATIENCE, shows_room)?;
        }
        windows_drawn(the_box, &directory)
    });
    checks.record("a new name reaches the other", || {
        alice.act(Command::SetName {
            name: "Alice B.".to_owned(),
        })?;
        bob.wait_for("with Alice B.", PATIENCE, |state| {
            names(state) == ["Alice B."]
        })?;
        Ok(())
    });
    checks.record("alice sees bob go", || {
        bob.reset()?;
        alice.wait_for("alone", PATIENCE, |state| state.participants.is_empty())?;
        alice.wait_for_snapshot("showing herself alone", PATIENCE, shows_room)?;
        bob.wait_for_snapshot("showing the join screen", PATIENCE, |snapshot| {
            snapshot.ui.as_ref().is_some_and(|ui| ui.screen == "join")
        })?;
        Ok(())
    });
    checks.record("bob joins from his window", || {
        type_into(the_box, "bob", "Bob", &invite)?;
        bob.wait_for_snapshot("in the room, shown", PATIENCE, shows_room)?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        Ok(())
    });
    checks.record("a restarted relay is rejoined", || {
        bob.join(&invite, "Bob")?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        process::run(crate::podman::podman(&[
            "restart",
            "--time",
            "1",
            &Pod::Relay.container("relay"),
        ]))?;
        ready::relay(the_box)?;
        for participant in [&mut alice, &mut bob] {
            participant.wait_for("connected again", PATIENCE, |state| {
                state.connection == Connection::Connected
            })?;
        }
        alice.wait_for("with Bob, once", PATIENCE, |state| names(state) == ["Bob"])?;
        bob.wait_for("with Alice B., once", PATIENCE, |state| {
            names(state) == ["Alice B."]
        })?;
        Ok(())
    });
    checks.record("a restarted client is the same participant", || {
        let before = alice
            .wait_for("in the room", Duration::ZERO, |_| true)?
            .me
            .key;
        process::run(crate::podman::podman(&[
            "restart",
            "--time",
            "10",
            &Pod::Client("alice").container("client"),
        ]))?;
        ready::client_up(the_box, "alice")?;
        alice = Participant::connect(the_box, "alice")?;
        let after = alice.join(&invite, "Alice B.")?.me.key;
        if after != before {
            return Err(Error::Failed(format!(
                "alice was {before} before, {after} after: her device secret wasn't kept"
            )));
        }
        bob.wait_for("with Alice B., once", PATIENCE, |state| {
            names(state) == ["Alice B."]
        })?;
        Ok(())
    });
    checks.record("an honest room rejects nobody", || {
        for participant in [&mut alice, &mut bob] {
            participant.wait_for("rejecting nobody", Duration::ZERO, |state| {
                state.rejected.is_empty()
            })?;
        }
        Ok(())
    });
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    Ok(())
}

/// The window shows the room the harness is in: its screen, and a tile for each participant,
/// this one first, in the roster's order.
fn shows_room(snapshot: &Snapshot) -> bool {
    let (Some(room), Some(ui)) = (&snapshot.room, &snapshot.ui) else {
        return false;
    };
    ui.screen == "room" && ui.tiles == keys(room)
}

fn keys(room: &RoomState) -> Vec<String> {
    std::iter::once(&room.me)
        .chain(&room.participants)
        .map(|participant| participant.key.to_string())
        .collect()
}

/// Types `name` and `invite` into `client`'s join screen, as a person would, and presses Enter:
/// the window gets the keyboard (Xvnc has no window manager to give it), the name field has the
/// focus when the screen shows, Tab moves to the invite's, and each is emptied first
/// (crates/mic-slint/ui/app.slint, `JoinScreen`).
fn type_into(the_box: &TestBox, client: &'static str, name: &str, invite: &Invite) -> Result<()> {
    let invite = invite.to_mic_url();
    // One xdotool for each `type`, which takes every argument after it as text.
    let steps: [&[&str]; 5] = [
        &[
            "search",
            "--sync",
            "--name",
            "^mic$",
            "windowfocus",
            "--sync",
            "key",
            "ctrl+a",
        ],
        &["type", "--delay", "10", name],
        &["key", "Tab", "ctrl+a"],
        &["type", "--delay", "2", &invite],
        &["key", "Return"],
    ];
    for step in steps {
        let command: Vec<&str> = ["env", "DISPLAY=:1", "xdotool"]
            .into_iter()
            .chain(step.iter().copied())
            .collect();
        let typed = the_box.exec(&view::container(client), &command)?;
        if !typed.status.success() {
            return Err(Error::Failed(format!(
                "xdotool {step:?} in {client}: {}",
                String::from_utf8_lossy(&typed.stderr).trim()
            )));
        }
    }
    Ok(())
}

/// The colours of a window's tiles: this participant's, and another's
/// (crates/mic-slint/ui/app.slint, `NameTile`).
const TILES: [[u8; 3]; 2] = [[0x2b, 0x4a, 0x6b], [0x3a, 0x3a, 0x44]];

/// A screenshot of each client's display, kept in the record, shows both kinds of tile, within
/// the patience: the window draws a moment after its harness says what it shows.
fn windows_drawn(the_box: &TestBox, directory: &Path) -> Result<()> {
    let tool = view::tool(the_box.repo(), "testbox-shot")?;
    for client in crate::podman::CLIENTS {
        let display = format!("{client}={}", the_box.view_socket(client).display());
        let path = directory.join(format!("roster-{client}.png"));
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            view::shot(&tool, directory, "roster", std::slice::from_ref(&display))?;
            let (pixels, channels) = crate::checks::png_pixels(&path)?;
            let missing = TILES.into_iter().find(|colour| {
                !pixels
                    .chunks_exact(channels)
                    .any(|pixel| pixel[..3] == *colour)
            });
            match missing {
                None => break,
                Some(colour) if std::time::Instant::now() > deadline => {
                    return Err(Error::Failed(format!(
                        "{}: no #{:02x}{:02x}{:02x} tile",
                        path.display(),
                        colour[0],
                        colour[1],
                        colour[2]
                    )));
                }
                Some(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
    Ok(())
}
