//! The `soak` phase (PLAN.md, commit 67): a long call, which nothing else runs (room-40 takes
//! minutes): alice and bob in their windows, and six of the crowd, all sending cameras, alice and
//! bob their tones, a message in the chat each minute. Every stretch, each window still hears
//! and sees the others, and the record keeps each mic's memory and CPU; at the end, what each
//! window holds may have grown only so much past what it held after the first stretch, once its
//! buffers filled. 30 minutes, or `MIC_SOAK_MINUTES`; not in a default round:
//! `cargo testbox run soak`.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{TestBox, camera, crowd_camera, crowd_name, tone};
use crate::view;

use super::Checks;
use super::room_40::process;

/// How long, unless `MIC_SOAK_MINUTES` says.
const MINUTES: u64 = 30;
/// How often it looks, and says.
const STRETCH: Duration = Duration::from_secs(5 * 60);
/// How many of the crowd join.
const CROWD: u32 = 6;
/// How long a join, a tone or a picture may take.
const PATIENCE: Duration = Duration::from_secs(30);
/// How much more a window may hold at the end than after the first stretch: a leak of a
/// picture a second would pass a 30 minutes' soak by far.
const GROWTH_MIB: f64 = 64.0;

const WINDOWS: [&str; 2] = ["alice", "bob"];

pub fn soak(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let minutes = std::env::var("MIC_SOAK_MINUTES")
        .ok()
        .and_then(|minutes| minutes.parse().ok())
        .unwrap_or(MINUTES);
    let mut windows = checks.require("alice and bob answer", || {
        WINDOWS
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
    let keys = checks.require("eight in a room, each seen", || {
        let mut keys = Vec::new();
        for window in &mut windows {
            let name = window.name.clone();
            keys.push(window.join(&invite, &name)?.me.key.to_string());
        }
        for (index, member) in crowd.iter_mut().enumerate() {
            let index = index as u32 + 1;
            keys.push(member.join(&invite, &crowd_name(index))?.me.key.to_string());
        }
        for window in &mut windows {
            window.wait_for("with everyone", PATIENCE, |state| {
                state.participants.len() == WINDOWS.len() + CROWD as usize - 1
            })?;
        }
        Ok(keys)
    })?;
    let mut lines = vec![format!(
        "soak: alice and bob, and {CROWD} of the crowd, {minutes} minutes"
    )];
    let started = Instant::now();
    let mut first: Option<Vec<f64>> = None;
    let mut last: Vec<f64> = Vec::new();
    let mut stretch = 0;
    while started.elapsed() < Duration::from_secs(minutes * 60) {
        stretch += 1;
        let until = (started + STRETCH * stretch).min(started + Duration::from_secs(minutes * 60));
        let before = sample(the_box)?;
        let at = Instant::now();
        while Instant::now() < until {
            windows[0].act(Command::Say {
                text: format!("minute {}", started.elapsed().as_secs() / 60),
            })?;
            std::thread::sleep(Duration::from_secs(60).min(until - Instant::now()));
        }
        let after = sample(the_box)?;
        let seconds = at.elapsed().as_secs_f64();
        let mib: Vec<f64> = after
            .iter()
            .map(|process| process.rss_kib / 1024.0)
            .collect();
        lines.push(format!(
            "minute {}: {}",
            started.elapsed().as_secs() / 60,
            WINDOWS
                .iter()
                .zip(before.iter().zip(&after))
                .map(|(client, (before, after))| format!(
                    "{client} {:.0}% of a core, {:.0} MiB",
                    (after.cpu - before.cpu) / seconds * 100.0,
                    after.rss_kib / 1024.0
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
        println!("{}", lines.last().cloned().unwrap_or_default());
        checks.record(
            &format!("stretch {stretch}: each still hears and sees"),
            || still_there(&mut windows, &keys),
        );
        first.get_or_insert_with(|| mib.clone());
        last = mib;
    }
    let report = lines.join("\n") + "\n";
    crate::error::write(&checks.record_directory().join("soak.txt"), &report)?;
    checks.record(
        &format!("what each window holds grew by {GROWTH_MIB} MiB at most"),
        || {
            let first = first.as_deref().unwrap_or_default();
            let grown: Vec<String> = WINDOWS
                .iter()
                .zip(first.iter().zip(&last))
                .filter(|(_, (first, last))| *last - *first > GROWTH_MIB)
                .map(|(client, (first, last))| format!("{client} {first:.0} → {last:.0} MiB"))
                .collect();
            if grown.is_empty() {
                Ok(())
            } else {
                Err(Error::Failed(format!("grew: {}", grown.join(", "))))
            }
        },
    );
    for participant in windows.iter_mut().chain(crowd.iter_mut()) {
        participant.reset()?;
    }
    Ok(())
}

/// Each window's mic, now.
fn sample(the_box: &TestBox) -> Result<Vec<super::room_40::Process>> {
    WINDOWS
        .iter()
        .map(|client| process(the_box, &view::container(client), "mic"))
        .collect()
}

/// Each window hears the other's tone, and decodes every other's camera, with its pattern,
/// its pictures still counting up.
fn still_there(windows: &mut [Participant], keys: &[String]) -> Result<()> {
    let senders: Vec<u8> = WINDOWS
        .iter()
        .filter_map(|client| camera(client))
        .chain((1..=CROWD).map(crowd_camera))
        .collect();
    for (at, window) in windows.iter_mut().enumerate() {
        let other = WINDOWS[1 - at];
        let heard = tone(other).ok_or("a window without a tone")? as f64;
        let own = tone(WINDOWS[at]).ok_or("a window without a tone")? as f64;
        window.wait_to_hear(heard, &[own], PATIENCE)?;
        let counted = |window: &mut Participant| -> Result<Vec<(String, u64, Option<u8>)>> {
            Ok(window
                .seen()?
                .cameras
                .into_iter()
                .map(|camera| (camera.key, camera.pictures, camera.sender))
                .collect())
        };
        let before = counted(window)?;
        std::thread::sleep(Duration::from_secs(3));
        let after = counted(window)?;
        for (key, sender) in keys.iter().zip(&senders) {
            if *key == keys[at] {
                continue;
            }
            let pictures = |seen: &[(String, u64, Option<u8>)]| {
                seen.iter()
                    .find(|(whose, ..)| whose == key)
                    .map(|(_, pictures, sender)| (*pictures, *sender))
            };
            match (pictures(&before), pictures(&after)) {
                (Some((before, _)), Some((after, Some(seen))))
                    if after > before && seen == *sender => {}
                found => {
                    return Err(Error::Failed(format!(
                        "{}: camera {sender} isn't coming in: {found:?}",
                        window.name
                    )));
                }
            }
        }
    }
    Ok(())
}
