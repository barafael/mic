//! The `recording` phase (PLAN.md, commit 49): alice, bob, carol and dave in a room, only alice
//! unmuted; bob records for 16 seconds. Every window says so while he does, his own among them,
//! and none once he stops. Five seconds in, alice shares her screen; five later she stops and
//! mutes. His recording, one WebM file, reads back in his pod with ffmpeg: VP9 and Opus, and a
//! picture of the call as it went: alice's camera while she speaks, her screen while she shares,
//! and once nobody speaks, a grid of everyone's.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{TestBox, camera};
use crate::view;

use super::Checks;

/// How long a change may take to show everywhere.
const PATIENCE: Duration = Duration::from_secs(15);
/// How long each part of the recording lasts: alice speaking, sharing, and nobody speaking,
/// which takes the recording's 3 s hold to show as the grid.
const SPEAKING: Duration = Duration::from_secs(5);
const SHARING: Duration = Duration::from_secs(5);
const SILENT: Duration = Duration::from_secs(6);
/// What the file must hold of its 16 seconds, at the least: 15 pictures a second and 50 packets,
/// less what the start and the end take.
const PICTURES: u64 = 200;
const PACKETS: u64 = 700;
/// When in each part it's looked at, in seconds from the part's start: once what it does has
/// taken hold (a share's first pictures come a second or more after it starts; the grid, past the
/// speaker's 3 s hold, after a mute reaches bob).
const LOOKS: [(&str, f64); 3] = [
    ("alice speaking", 3.0),
    ("alice sharing", 5.0 + 3.5),
    ("nobody speaking", 5.0 + 5.0 + 5.5),
];
/// Where the windows record, in their pods (testbox/pods/client.yaml).
const RECORDINGS: &str = "/run/mic/recordings";

pub fn recording(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let names = ["alice", "bob", "carol", "dave"];
    let mut clients = checks.require("alice, bob, carol and dave answer", || {
        names
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
            if name != "alice" {
                client.act(Command::SetMuted { muted: true })?;
            }
        }
        for client in &mut clients {
            client.wait_for("with the others", PATIENCE, |state| {
                state.participants.len() == 3
            })?;
        }
        Ok(keys)
    })?;
    let bobs = keys[1].clone();
    // What was there before, so the new recording is told apart.
    let before = recordings(the_box)?;
    // When the recording started: the parts below start once every window says it records, which
    // a loaded host takes seconds to, and the file's timeline starts here.
    let mut started = Instant::now();
    let mut lead = Duration::ZERO;
    checks.record("bob records: every window says so, his own too", || {
        started = Instant::now();
        clients[1].act(Command::Record { on: true })?;
        for client in &mut clients {
            let own = client.name == "bob";
            client.wait_for_snapshot("saying bob records", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| {
                    ui.recorders == [bobs.clone()]
                        && ui.recording.as_deref().is_some_and(|said| {
                            if own {
                                said.starts_with("You're recording")
                            } else {
                                said == "bob is recording this call."
                            }
                        })
                })
            })?;
        }
        Ok(())
    });
    checks.record("he stops: no window says anyone records", || {
        lead = started.elapsed();
        std::thread::sleep(SPEAKING);
        clients[0].act(Command::ShareScreen { on: true })?;
        std::thread::sleep(SHARING);
        clients[0].act(Command::ShareScreen { on: false })?;
        clients[0].act(Command::SetMuted { muted: true })?;
        std::thread::sleep(SILENT);
        clients[1].act(Command::Record { on: false })?;
        for client in &mut clients {
            client.wait_for_snapshot("saying nobody records", PATIENCE, |snapshot| {
                snapshot
                    .ui
                    .as_ref()
                    .is_some_and(|ui| ui.recording.is_none() && ui.recorders.is_empty())
            })?;
        }
        Ok(())
    });
    checks.record(
        "his recording plays: alice's camera, her screen, then a grid of everyone",
        || {
            let made: Vec<String> = recordings(the_box)?
                .into_iter()
                .filter(|file| !before.contains(file))
                .collect();
            let [file] = made.as_slice() else {
                return Err(Error::Failed(format!("new recordings: {made:?}")));
            };
            let file = format!("{RECORDINGS}/{file}");
            let probed = run(
                the_box,
                &[
                    "ffprobe",
                    "-v",
                    "error",
                    "-count_frames",
                    "-show_entries",
                    "stream=codec_name,nb_read_frames",
                    "-of",
                    "csv=p=0",
                    &file,
                ],
            )?;
            let frames = |codec: &str| -> u64 {
                probed
                    .lines()
                    .find_map(|line| line.strip_prefix(&format!("{codec},")))
                    .and_then(|count| count.trim().parse().ok())
                    .unwrap_or(0)
            };
            let (pictures, packets) = (frames("vp9"), frames("opus"));
            if pictures < PICTURES || packets < PACKETS {
                return Err(Error::Failed(format!(
                    "{pictures} pictures and {packets} packets, of {PICTURES} and {PACKETS}"
                )));
            }
            let mut wrong = Vec::new();
            let alices = camera("alice").ok_or("no camera")?;
            for (part, after) in LOOKS {
                let at = format!("{:.1}", lead.as_secs_f64() + after);
                let picture = picture_at(the_box, &file, &at)?;
                let read = mic_media::video::read_pattern(&picture).map(|pattern| pattern.sender);
                let right = match part {
                    "alice speaking" => read == Some(alices),
                    "alice sharing" => read == Some(mic_harness::synthetic::screen_pattern(alices)),
                    // A grid: no one picture, and each camera's colour in it.
                    _ => {
                        read.is_none()
                            && ["alice", "bob", "dave"].into_iter().all(|client| {
                                camera(client)
                                    .is_some_and(|sender| tinted(&picture, sender) >= GRID_TINT)
                            })
                    }
                };
                if !right {
                    wrong.push(format!(
                        "at {at} s, {part}: the pattern of {read:?}, and of colours {:?}",
                        ["alice", "bob", "dave"]
                            .map(|client| camera(client).map(|sender| tinted(&picture, sender)))
                    ));
                }
            }
            if wrong.is_empty() {
                Ok(())
            } else {
                Err(Error::Failed(wrong.join("; ")))
            }
        },
    );
    for client in &mut clients {
        client.reset()?;
    }
    Ok(())
}

/// How many chroma samples of a camera's colour make it in the grid: a tile's worth, a ninth of
/// the picture's 230 400, and then some.
const GRID_TINT: usize = 5_000;

/// The recording's picture at `at` seconds, decoded by ffmpeg in bob's pod.
fn picture_at(the_box: &TestBox, file: &str, at: &str) -> Result<mic_media::video::Picture> {
    let settings = mic_core::recording::RECORDING_SETTINGS;
    let ran = the_box.exec(
        &view::container("bob"),
        &[
            "ffmpeg",
            "-v",
            "error",
            "-ss",
            at,
            "-i",
            file,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ],
    )?;
    let size = (settings.width * settings.height * 3 / 2) as usize;
    if !ran.status.success() || ran.stdout.len() != size {
        return Err(Error::Failed(format!(
            "the picture at {at} s: {} bytes, {}",
            ran.stdout.len(),
            String::from_utf8_lossy(&ran.stderr).trim()
        )));
    }
    Ok(mic_media::video::Picture {
        width: settings.width,
        height: settings.height,
        data: ran.stdout,
    })
}

/// How many of `picture`'s chroma samples are of `sender`'s camera's colour.
fn tinted(picture: &mic_media::video::Picture, sender: u8) -> usize {
    let (u, v) = mic_media::video::tint(sender);
    let luma = (picture.width * picture.height) as usize;
    let (us, vs) = picture.data[luma..].split_at(luma / 4);
    us.iter()
        .zip(vs)
        .filter(|(at_u, at_v)| at_u.abs_diff(u) <= 6 && at_v.abs_diff(v) <= 6)
        .count()
}

/// The recordings in bob's pod, by file.
fn recordings(the_box: &TestBox) -> Result<Vec<String>> {
    let listed = run(
        the_box,
        &[
            "sh",
            "-c",
            &format!("mkdir -p {RECORDINGS} && ls {RECORDINGS}"),
        ],
    )?;
    Ok(listed.lines().map(str::to_owned).collect())
}

/// What `command` says, run in bob's pod; it failing is an error.
fn run(the_box: &TestBox, command: &[&str]) -> Result<String> {
    let ran = the_box.exec(&view::container("bob"), command)?;
    if ran.status.success() {
        Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
    } else {
        Err(Error::Failed(format!(
            "{command:?} in bob's pod: {}",
            String::from_utf8_lossy(&ran.stderr).trim()
        )))
    }
}
