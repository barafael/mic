//! The `recording` phase (PLAN.md, commit 49): alice, bob, carol and dave in a room; bob records
//! for five seconds. Every window says so while he does, his own among them, and none once he
//! stops; and his recording, a WebM file a participant, reads back in his pod with ffprobe: each
//! camera in VP9 and each voice in Opus, as they were sent, about five seconds of each.

use std::time::Duration;

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{TestBox, camera, tone};
use crate::view;

use super::Checks;

/// How long a change may take to show everywhere.
const PATIENCE: Duration = Duration::from_secs(15);
/// How long bob records.
const RECORDED: Duration = Duration::from_secs(5);
/// What a participant's file must hold of the five seconds, at the least: 15 pictures a second
/// and 50 packets, less what the start and the end take.
const PICTURES: u64 = 50;
const PACKETS: u64 = 150;
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
    checks.record("bob records: every window says so, his own too", || {
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
        std::thread::sleep(RECORDED);
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
        "his recording reads back: each one's camera in VP9 and voice in Opus",
        || {
            let made: Vec<String> = recordings(the_box)?
                .into_iter()
                .filter(|directory| !before.contains(directory))
                .collect();
            let [directory] = made.as_slice() else {
                return Err(Error::Failed(format!("new recordings: {made:?}")));
            };
            let files = run(
                the_box,
                &["sh", "-c", &format!("ls {RECORDINGS}/{directory}")],
            )?;
            let files: Vec<&str> = files.lines().collect();
            let mut wrong = Vec::new();
            for name in names {
                let Some(file) = files.iter().find(|file| file.starts_with(&format!("{name}-")))
                else {
                    wrong.push(format!("no file of {name}'s in {files:?}"));
                    continue;
                };
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
                        &format!("{RECORDINGS}/{directory}/{file}"),
                    ],
                )?;
                let frames = |codec: &str| -> u64 {
                    probed
                        .lines()
                        .find_map(|line| line.strip_prefix(&format!("{codec},")))
                        .and_then(|count| count.trim().parse().ok())
                        .unwrap_or(0)
                };
                // Who sends what: carol has no camera, dave no voice (podman::tone, camera).
                let (pictures, packets) = (frames("vp9"), frames("opus"));
                let wants_camera = camera(name).is_some();
                let wants_voice = tone(name).is_some() || name == "carol";
                if (wants_camera && pictures < PICTURES) || (wants_voice && packets < PACKETS) {
                    wrong.push(format!(
                        "{file}: {pictures} pictures and {packets} packets, of {PICTURES} and {PACKETS}"
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

/// The recordings in bob's pod, by directory.
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
