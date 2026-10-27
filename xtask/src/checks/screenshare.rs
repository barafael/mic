//! The `screenshare` phase (PLAN.md, commit 45): alice shares a synthetic screen, a pattern of
//! her own (mic-harness's `screen_pattern`): bob and dave decode it, it's hers, and their windows
//! show it on the stage, in its colour; once she stops, nobody shows it and no more of it comes.
//! Then carol, who has the box's real devices, shares her real display, captured through X11
//! (mic-media's `screen`): the others decode it.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{TestBox, camera};
use crate::view;

use super::Checks;

/// How long a change may take to show everywhere.
const PATIENCE: Duration = Duration::from_secs(15);
/// How many of a screen's pictures a viewer must decode to count as seeing it: a few seconds'
/// worth at its 5 a second.
const PICTURES: u64 = 10;
/// How many pixels of the screen's colour make it shown on the stage: more than a tile has.
const STAGED: usize = 60_000;

pub fn screenshare(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let mut clients = checks.require("alice, bob, carol and dave answer", || {
        ["alice", "bob", "carol", "dave"]
            .into_iter()
            .map(|client| Participant::connect(the_box, client))
            .collect::<Result<Vec<_>>>()
    })?;
    let invite = Invite::new_room();
    let keys = checks.require("they're in a room", || {
        let mut keys = Vec::new();
        for client in &mut clients {
            let name = client.name.clone();
            keys.push(client.join(&invite, &name)?.me.key.to_string());
        }
        for client in &mut clients {
            client.wait_for("with the others", PATIENCE, |state| {
                state.participants.len() == 3
            })?;
        }
        Ok(keys)
    })?;
    let (alice_key, carol_key) = (keys[0].clone(), keys[2].clone());
    let pattern = mic_harness::synthetic::screen_pattern(camera("alice").ok_or("no camera")?);
    let directory = checks.record_directory().to_owned();
    checks.record(
        "alice shares: bob and dave decode her screen, and show it on the stage",
        || {
            clients[0].act(Command::ShareScreen { on: true })?;
            let tool = view::tool(the_box.repo(), "testbox-shot")?;
            let staged = super::video::colour(pattern);
            for viewer in [1, 3] {
                let viewer = &mut clients[viewer];
                sees_screen(viewer, &alice_key, Some(pattern))?;
                viewer.wait_for_snapshot("showing her screen", PATIENCE, |snapshot| {
                    snapshot
                        .ui
                        .as_ref()
                        .is_some_and(|ui| ui.screens == [alice_key.clone()])
                })?;
                let client: &'static str = if viewer.name == "bob" { "bob" } else { "dave" };
                let deadline = Instant::now() + PATIENCE;
                loop {
                    let shown =
                        super::video::colours(the_box, &tool, &directory, client, &[staged])?[0];
                    if shown >= STAGED {
                        break;
                    }
                    if Instant::now() > deadline {
                        return Err(Error::Failed(format!(
                            "{client}'s display: the screen's colour in {shown} pixels, of {STAGED}"
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            clients[0].wait_for_snapshot("saying she shares", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| ui.sharing)
            })?;
            Ok(())
        },
    );
    checks.record(
        "she stops: nobody shows it, and no more of it comes",
        || {
            clients[0].act(Command::ShareScreen { on: false })?;
            for viewer in [1, 3] {
                clients[viewer].wait_for_snapshot("showing no screen", PATIENCE, |snapshot| {
                    snapshot.ui.as_ref().is_some_and(|ui| ui.screens.is_empty())
                })?;
            }
            std::thread::sleep(Duration::from_millis(500));
            let before = pictures(&mut clients[1], &alice_key)?;
            std::thread::sleep(Duration::from_secs(2));
            let after = pictures(&mut clients[1], &alice_key)?;
            if after != before {
                return Err(Error::Failed(format!(
                    "{} of her screen's pictures came after she stopped",
                    after - before
                )));
            }
            Ok(())
        },
    );
    checks.record(
        "carol shares her real display, captured through X11: the others decode it",
        || {
            clients[2].act(Command::ShareScreen { on: true })?;
            for viewer in [0, 1, 3] {
                sees_screen(&mut clients[viewer], &carol_key, None)?;
            }
            clients[2].act(Command::ShareScreen { on: false })?;
            Ok(())
        },
    );
    for client in &mut clients {
        client.reset()?;
    }
    Ok(())
}

/// How many of `whose` screen's pictures `viewer` decoded.
fn pictures(viewer: &mut Participant, whose: &str) -> Result<u64> {
    Ok(viewer
        .seen()?
        .screens
        .iter()
        .find(|screen| screen.key == whose)
        .map_or(0, |screen| screen.pictures))
}

/// Waits until `viewer` decodes [`PICTURES`] more of `whose` screen, showing `pattern`'s if it's
/// given.
fn sees_screen(viewer: &mut Participant, whose: &str, pattern: Option<u8>) -> Result<()> {
    let before = pictures(viewer, whose)?;
    let deadline = Instant::now() + PATIENCE;
    loop {
        let seen = viewer.seen()?;
        let screen = seen.screens.iter().find(|screen| screen.key == whose);
        let decoded = screen.map_or(0, |screen| screen.pictures);
        let theirs = pattern
            .is_none_or(|pattern| screen.is_some_and(|screen| screen.sender == Some(pattern)));
        if decoded >= before + PICTURES && theirs {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "{}: {} of {whose}'s screen's pictures within {}s: {screen:?}",
                viewer.name,
                decoded - before,
                PATIENCE.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
