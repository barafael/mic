//! The `room` phase (PLAN.md, commits 43 and 44): the six windows and six of the crowd in one
//! room, twelve, more than a window's nine tiles. Each window shows nine and a "+3", and decodes
//! the cameras of those it shows only; one who speaks while not shown comes into view, ringed;
//! and what one says in the chat, a reaction, a raised hand reach everyone.
//!
//! The crowd joins first, then carol, dave, erin and frank, then alice and bob, who alone have
//! voices: in dave's window, whose nine are he and the first eight others, alice isn't shown
//! until she speaks. (carol decodes no video in the box, so dave's window is the one looked at.)

use std::time::{Duration, Instant};

use mic_core::chat::Reaction;
use mic_core::{Command, Invite};
use mic_harness::Snapshot;

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{CLIENTS, TestBox, crowd_name};

use super::Checks;

/// How long a change may take to show everywhere.
const PATIENCE: Duration = Duration::from_secs(20);
/// How many of the crowd join.
const CROWD: u32 = 6;
/// The windows, in the order they join.
const JOINING: [&str; 6] = ["carol", "dave", "erin", "frank", "alice", "bob"];

pub fn room(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    if the_box.crowd() < CROWD {
        return Err(Error::Failed(format!(
            "a crowd of {} in the box, of {CROWD} wanted: `cargo testbox up --crowd {CROWD}` or more",
            the_box.crowd()
        )));
    }
    let mut windows = checks.require("the windows answer", || {
        JOINING
            .iter()
            .map(|client| Participant::connect(the_box, client))
            .collect::<Result<Vec<_>>>()
    })?;
    let mut crowd = checks.require("the crowd answers", || {
        (1..=CROWD)
            .map(|index| Participant::crowd(the_box, index))
            .collect::<Result<Vec<_>>>()
    })?;
    let invite = Invite::new_room();
    let everyone = CROWD as usize + CLIENTS.len();
    // Each window's key, by name.
    let keys = checks.require("twelve in one room", || {
        for (index, member) in crowd.iter_mut().enumerate() {
            member.join(&invite, &crowd_name(index as u32 + 1))?;
        }
        let mut keys = Vec::new();
        for window in &mut windows {
            // Only alice and bob speak, and only when asked to.
            let name = window.name.clone();
            let state = window.join(&invite, &capitalized(&name))?;
            window.act(Command::SetMuted { muted: true })?;
            keys.push((name, state.me.key.to_string()));
        }
        for window in &mut windows {
            window.wait_for("with everyone", PATIENCE, |state| {
                state.participants.len() == everyone - 1
            })?;
        }
        Ok(keys)
    })?;
    let key = |name: &str| {
        keys.iter()
            .find(|(window, _)| window == name)
            .map(|(_, key)| key.clone())
            .unwrap_or_default()
    };
    checks.record("each window shows nine tiles and a +3", || {
        for window in &mut windows {
            window.wait_for_snapshot("showing nine and a +3", PATIENCE, |snapshot| {
                ui(snapshot).is_some_and(|(tiles, overflow)| tiles == 9 && overflow == 3)
            })?;
        }
        Ok(())
    });
    checks.record(
        "dave decodes the cameras of the tiles he shows, and none else",
        || {
            let dave_key = key("dave");
            let dave = &mut windows[1];
            // Measured again if the tiles change while it's measured: a voice heard a moment
            // before its mute took hold makes a speaker, who comes into view.
            let mut tries = 0;
            loop {
                tries += 1;
                let (shown, cameras) = shown_and_cameras(dave)?;
                // What was on its way when the tiles were chosen.
                std::thread::sleep(Duration::from_secs(2));
                let before = dave.seen()?;
                std::thread::sleep(Duration::from_secs(3));
                let after = dave.seen()?;
                if shown_and_cameras(dave)?.0 != shown && tries < 3 {
                    continue;
                }
                let grew = |key: &str| {
                    let pictures = |seen: &mic_harness::Seen| {
                        seen.cameras
                            .iter()
                            .find(|camera| camera.key == key)
                            .map_or(0, |camera| camera.pictures)
                    };
                    pictures(&after) > pictures(&before)
                };
                let mut wrong = Vec::new();
                for camera in after
                    .cameras
                    .iter()
                    .filter(|camera| camera.key != dave_key && cameras.contains(&camera.key))
                {
                    if grew(&camera.key) != shown.contains(&camera.key) {
                        wrong.push(format!(
                            "{} ({}): shown {}, decoded {}",
                            camera.key,
                            camera
                                .sender
                                .map_or("?".to_owned(), |sender| sender.to_string()),
                            shown.contains(&camera.key),
                            grew(&camera.key)
                        ));
                    }
                }
                let decoding = after
                    .cameras
                    .iter()
                    .filter(|camera| grew(&camera.key))
                    .count();
                if !wrong.is_empty() || decoding == 0 {
                    return Err(Error::Failed(format!(
                        "{decoding} cameras decoded: {}",
                        wrong.join("; ")
                    )));
                }
                return Ok(());
            }
        },
    );
    checks.record(
        "alice speaks: she's the speaker everywhere, and dave's window brings her in, ringed",
        || {
            let alice_key = key("alice");
            let alice = windows
                .iter_mut()
                .find(|window| window.name == "alice")
                .ok_or("no alice")?;
            alice.act(Command::SetMuted { muted: false })?;
            for window in windows.iter_mut().filter(|window| window.name != "alice") {
                window.wait_for("alice the speaker", PATIENCE, |state| {
                    state
                        .speaker
                        .is_some_and(|speaker| speaker.to_string() == alice_key)
                })?;
            }
            let dave = &mut windows[1];
            dave.wait_for_snapshot("alice ringed among nine", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| {
                    ui.tiles.len() == 9
                        && ui.tiles.contains(&alice_key)
                        && ui.speaking.contains(&alice_key)
                })
            })?;
            // And her camera's decoded, now that it shows.
            let pictures = |dave: &mut Participant| -> Result<u64> {
                Ok(dave
                    .seen()?
                    .cameras
                    .iter()
                    .find(|camera| camera.key == alice_key)
                    .map_or(0, |camera| camera.pictures))
            };
            let before = pictures(dave)?;
            let deadline = Instant::now() + PATIENCE;
            while pictures(dave)? < before + 15 {
                if Instant::now() > deadline {
                    return Err(Error::Failed(
                        "dave shows alice but doesn't decode her camera".to_owned(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let alice = windows
                .iter_mut()
                .find(|window| window.name == "alice")
                .ok_or("no alice")?;
            alice.act(Command::SetMuted { muted: true })?;
            Ok(())
        },
    );
    checks.record(
        "a message typed into alice's chat reaches every window's",
        || {
            super::window::type_message(the_box, "alice", "hello, room")?;
            for window in &mut windows {
                window.wait_for_snapshot("showing alice's message", PATIENCE, |snapshot| {
                    snapshot
                        .ui
                        .as_ref()
                        .is_some_and(|ui| ui.chat == ["Alice: hello, room"])
                })?;
            }
            Ok(())
        },
    );
    checks.record(
        "bob's clap and dave's hand reach everyone, and show on their tiles and in the panel",
        || {
            let (bob_key, dave_key) = (key("bob"), key("dave"));
            for window in &mut windows {
                match window.name.as_str() {
                    "bob" => window.act(Command::React {
                        reaction: Reaction::Clap,
                    })?,
                    "dave" => window.act(Command::SetHand { raised: true })?,
                    _ => {}
                }
            }
            let clap = format!("{bob_key} clap");
            for window in &mut windows {
                window.wait_for("bob's clap and dave's hand", PATIENCE, |state| {
                    let everyone = || std::iter::once(&state.me).chain(&state.participants);
                    state.reactions.iter().any(|reacted| {
                        reacted.from.to_string() == bob_key && reacted.reaction == Reaction::Clap
                    }) && everyone()
                        .any(|them| them.key.to_string() == dave_key && them.profile.hand)
                })?;
                // On their tiles, where they have one; dave's hand in the participants panel,
                // which lists all twelve, those behind the "+N" among them.
                window.wait_for_snapshot(
                    "their tiles and the panel saying so",
                    PATIENCE,
                    |snapshot| {
                        snapshot.ui.as_ref().is_some_and(|ui| {
                            (!ui.tiles.contains(&bob_key) || ui.reactions.contains(&clap))
                                && (!ui.tiles.contains(&dave_key) || ui.hands.contains(&dave_key))
                                && ui.people.len() == 12
                                && ui.people_hands.contains(&dave_key)
                        })
                    },
                )?;
            }
            Ok(())
        },
    );
    for participant in windows.iter_mut().chain(crowd.iter_mut()) {
        participant.reset()?;
    }
    Ok(())
}

/// The tiles `window` shows, by key, and who in its room sends a camera, by their catalogs.
fn shown_and_cameras(window: &mut Participant) -> Result<(Vec<String>, Vec<String>)> {
    let snapshot = window.snapshot()?;
    let shown = snapshot
        .ui
        .map(|ui| ui.tiles)
        .ok_or_else(|| Error::Failed(format!("{}'s window says nothing", window.name)))?;
    let cameras = snapshot
        .room
        .ok_or_else(|| Error::Failed(format!("{} is in no room", window.name)))?
        .participants
        .iter()
        .filter(|them| !them.renditions.is_empty())
        .map(|them| them.key.to_string())
        .collect();
    Ok((shown, cameras))
}

/// What `snapshot`'s window shows: how many tiles, and how many beyond them.
fn ui(snapshot: &Snapshot) -> Option<(usize, usize)> {
    snapshot.ui.as_ref().map(|ui| (ui.tiles.len(), ui.overflow))
}

/// `name` with a capital: the name a window joins under.
fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
