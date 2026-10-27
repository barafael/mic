//! The `audio-devices` phase: the path through real devices (cpal, ALSA, PipeWire) both ways.
//! carol uses her pod's PipeWire speaker and microphone (testbox/client/box-devices.conf), alice
//! her tone and analyzer. What carol's speaker plays is recorded from its monitor and must hold
//! alice's tone; a tone played into carol's microphone must reach alice; and with carol's speaker
//! looped into her microphone, as a laptop without a headset does, alice must not hear her own
//! tone come back, its echo cancelled in carol's mic (D14). It measures, so it runs alone.

use std::path::Path;
use std::time::{Duration, Instant};

use mic_core::Invite;
use mic_media::audio::{SAMPLE_RATE, level};

use crate::error::{Error, Result};
use crate::harness::{LOUD, Participant, names};
use crate::podman::{RUN, TestBox, tone};
use crate::view;

use super::Checks;

/// How long a roster, or what's heard, may take to show a change.
const PATIENCE: Duration = Duration::from_secs(15);
/// The tone played into carol's microphone, apart from alice's and bob's.
const CAROLS: f64 = 770.0;
/// What a recording of the speaker leaves out at its start: the stream settling.
const SETTLE: usize = SAMPLE_RATE as usize / 5;

pub fn audio_devices(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut carol) = checks.require("alice and carol answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "carol")?,
        ))
    })?;
    let alices = f64::from(tone("alice").ok_or("alice sends no tone")?);
    let invite = Invite::new_room();
    checks.require("alice and carol are in a room", || {
        alice.join(&invite, "Alice")?;
        carol.join(&invite, "Carol")?;
        alice.wait_for("with Carol", PATIENCE, |state| names(state) == ["Carol"])?;
        carol.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        Ok(())
    })?;
    checks.record("carol's speaker plays alice's voice", || {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let played = record(the_box, "carol", "speaker", true)?;
            let heard = level(&played[SETTLE.min(played.len())..], alices);
            if heard >= LOUD {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(Error::Failed(format!(
                    "{alices} Hz at {heard} in what carol's speaker played"
                )));
            }
        }
    });
    checks.record("carol's microphone reaches alice", || {
        play_into_microphone(the_box, "carol", CAROLS, PATIENCE + Duration::from_secs(5))?;
        alice.wait_to_hear(CAROLS, &[alices], PATIENCE)
    });
    checks.record(
        "with carol's speaker in her microphone, alice hears no echo",
        || {
            loopback(the_box, "carol", true)?;
            let echoed = (|| {
                // The loop carries the echo: carol's microphone, unprocessed, holds alice's tone.
                let looped = record(the_box, "carol", "microphone", false)?;
                let looped = level(&looped[SETTLE.min(looped.len())..], alices);
                if looped < LOUD {
                    return Err(Error::Failed(format!(
                        "the loop carries no echo: {alices} Hz at {looped} in carol's microphone"
                    )));
                }
                // Alice hears none of it come back, once the canceller has converged, and for a
                // while after.
                alice.wait_for_silence(&[alices], PATIENCE)?;
                for _ in 0..ECHO_CHECKS {
                    std::thread::sleep(Duration::from_millis(500));
                    alice.wait_for_silence(&[alices], Duration::ZERO)?;
                }
                Ok(())
            })();
            loopback(the_box, "carol", false)?;
            echoed
        },
    );
    for participant in [&mut alice, &mut carol] {
        participant.reset()?;
    }
    Ok(())
}

/// How many more times, half a second apart, alice must hear no echo once she first doesn't.
const ECHO_CHECKS: usize = 6;

/// Links `client`'s speaker's monitor into its microphone (`on`), or unlinks it.
fn loopback(the_box: &TestBox, client: &'static str, on: bool) -> Result<()> {
    let mut command = vec!["pw-link"];
    if !on {
        command.push("--disconnect");
    }
    command.extend(["speaker:monitor_MONO", "microphone-in:playback_MONO"]);
    let linked = the_box.exec(&view::container(client), &command)?;
    if linked.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "pw-link {command:?} in {client}: {}",
            String::from_utf8_lossy(&linked.stderr).trim()
        )))
    }
}

/// A second of `target` in `client`'s PipeWire, a sink's monitor if `sink`, else a source: mono
/// `f32` at 48 kHz, through a file in its run directory, which the host reads.
fn record(the_box: &TestBox, client: &'static str, target: &str, sink: bool) -> Result<Vec<f32>> {
    let file = format!("{target}.raw");
    let into = format!("/run/mic/{file}");
    // A sink's monitor is recorded only when asked for.
    let properties = format!("{{ stream.capture.sink = {sink} }}");
    let recorded = the_box.exec(
        &view::container(client),
        &[
            "timeout",
            "1.2",
            "pw-record",
            "-P",
            &properties,
            "--raw",
            "--target",
            target,
            "--rate",
            "48000",
            "--channels",
            "1",
            "--format",
            "f32",
            &into,
        ],
    )?;
    // `timeout` ends it: 124 is the recording done.
    if !matches!(recorded.status.code(), Some(0 | 124)) {
        return Err(Error::Failed(format!(
            "pw-record in {client}: {}",
            String::from_utf8_lossy(&recorded.stderr).trim()
        )));
    }
    let path = the_box.repo().join(RUN).join(client).join(&file);
    let bytes = crate::error::read(&path)?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&sample| f32::from_le_bytes(sample))
        .collect())
}

/// Plays `frequency` Hz into `client`'s microphone for `duration`, in the background: a raw
/// file written to its run directory, which `pw-play` plays into the sink that feeds it.
fn play_into_microphone(
    the_box: &TestBox,
    client: &'static str,
    frequency: f64,
    duration: Duration,
) -> Result<()> {
    let file = "microphone.raw";
    let count = (duration.as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
    let bytes: Vec<u8> = (0..count)
        .flat_map(|i| {
            let at = i as f64 / f64::from(SAMPLE_RATE);
            ((at * frequency * std::f64::consts::TAU).sin() as f32 * 0.5).to_le_bytes()
        })
        .collect();
    let path: &Path = &the_box.repo().join(RUN).join(client).join(file);
    crate::error::write(path, bytes)?;
    let command = format!(
        "pw-play --raw --target microphone-in --rate 48000 --channels 1 --format f32 \
         /run/mic/{file} >/dev/null 2>&1 &"
    );
    let started = the_box.exec(&view::container(client), &["sh", "-c", &command])?;
    if started.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "pw-play in {client}: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        )))
    }
}
