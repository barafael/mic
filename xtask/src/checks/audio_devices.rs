//! The `audio-devices` phase: the path through real devices (cpal, ALSA, PipeWire) both ways.
//! carol uses her pod's PipeWire speaker and microphone (testbox/client/box-devices.conf), alice
//! her tone and analyzer. What carol's speaker plays is recorded from its monitor and must hold
//! alice's tone; a tone played into carol's microphone must reach alice; and with carol's speaker
//! looped into her microphone, as a laptop without a headset does, alice must not hear her own
//! tone come back, its echo cancelled in carol's mic (D14). And with a speech-like voice over
//! noise in carol's microphone, each noise suppressor is measured by what alice hears, and the
//! one mic uses must raise the voice over the noise by at least [`MIN_GAIN`] (D14, PLAN.md commit
//! 30). It measures, so it runs alone.

use std::path::Path;
use std::time::{Duration, Instant};

use mic_core::{Command, Invite};
use mic_media::audio::noise::{
    NoiseSuppression, measured_frequencies, noise, speech_like, voice_power, voice_to_noise,
};
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
        let duration = PATIENCE + Duration::from_secs(5);
        let count = (duration.as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
        let tone: Vec<f32> = (0..count)
            .map(|i| {
                let at = i as f64 / f64::from(SAMPLE_RATE);
                ((at * CAROLS * std::f64::consts::TAU).sin() * 0.5) as f32
            })
            .collect();
        play_into_microphone(the_box, "carol", &tone)?;
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
    let directory = checks.record_directory().to_owned();
    checks.record(
        "noise suppression raises carol's voice over her noise",
        || {
            let measured = measure_suppressors(the_box, &mut alice, &mut carol)?;
            let report: String = measured
                .iter()
                .map(|(mode, ratio, voice)| {
                    format!("{mode:?}: voice to noise {ratio:.1} dB, voice {voice:.1} dB\n")
                })
                .collect();
            print!("{report}");
            crate::error::write(&directory.join("noise-suppression.txt"), &report)?;
            let of = |wanted: NoiseSuppression| {
                measured
                    .iter()
                    .find(|(mode, _, _)| *mode == wanted)
                    .map(|&(_, ratio, voice)| (ratio, voice))
                    .ok_or("not measured")
            };
            let (off, voice_off) = of(NoiseSuppression::Off)?;
            let (used, voice_used) = of(USED)?;
            if used - off < MIN_GAIN || voice_used < voice_off - MAX_VOICE_LOSS {
                return Err(Error::Failed(format!(
                    "{USED:?} raised the voice over the noise by {:.1} dB (at least {MIN_GAIN} dB \
                 wanted), and changed the voice by {:.1} dB (at most -{MAX_VOICE_LOSS} dB)",
                    used - off,
                    voice_used - voice_off
                )));
            }
            Ok(())
        },
    );
    for participant in [&mut alice, &mut carol] {
        participant.reset()?;
    }
    Ok(())
}

/// The suppressor mic uses (crates/mic-slint, `devices::NOISE_SUPPRESSION`).
const USED: NoiseSuppression = NoiseSuppression::Rnnoise;
/// What it must raise the voice over the noise by, at the far end, in dB: below what it measured
/// here (11.5 to 12.3 dB over three runs, 2026-10-27), so a regression shows, not the run's spread.
const MIN_GAIN: f64 = 8.0;
/// What it may take of the voice itself, in dB (it took 0.1 to 0.3).
const MAX_VOICE_LOSS: f64 = 3.0;
/// How long a suppressor runs before it's measured: what it takes to learn the noise, and for
/// alice's analyzer's second to hold only what it let through.
const SETTLING: Duration = Duration::from_secs(4);
/// How many times each is measured, half a second apart, and averaged.
const SAMPLES: usize = 4;

/// Each suppressor's voice-to-noise ratio and voice power, in dB, as alice hears carol, with
/// the speech-like voice over noise played into carol's microphone.
fn measure_suppressors(
    the_box: &TestBox,
    alice: &mut Participant,
    carol: &mut Participant,
) -> Result<Vec<(NoiseSuppression, f64, f64)>> {
    let modes = [
        NoiseSuppression::Off,
        NoiseSuppression::Rnnoise,
        NoiseSuppression::Sonora,
    ];
    let each = SETTLING + Duration::from_millis(500) * SAMPLES as u32;
    let duration = each * modes.len() as u32 + Duration::from_secs(3);
    let count = (duration.as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
    let voice: Vec<f32> = speech_like(0, count)
        .iter()
        .zip(noise(1, count, 0.02))
        .map(|(voice, noise)| voice + noise)
        .collect();
    // Alice muted: with her tone playing at carol's speaker, carol's echo canceller, fresh from
    // the loop it cancelled, takes a quiet voice talking over it for echo for its first several
    // seconds (AEC3 settles to transparent within 20 s with no echo). That isn't noise
    // suppression, so it's left out of what's measured.
    alice.act(Command::SetMuted { muted: true })?;
    play_into_microphone(the_box, "carol", &voice)?;
    let frequencies = measured_frequencies();
    let mut measured = Vec::new();
    for mode in modes {
        carol.act(Command::SetNoiseSuppression { mode })?;
        std::thread::sleep(SETTLING);
        let mut levels = vec![0.0; frequencies.len()];
        for _ in 0..SAMPLES {
            std::thread::sleep(Duration::from_millis(500));
            let heard = alice.heard(&frequencies)?;
            for (sum, level) in levels.iter_mut().zip(heard.levels) {
                *sum += level / SAMPLES as f64;
            }
        }
        measured.push((mode, voice_to_noise(&levels), voice_power(&levels)));
    }
    alice.act(Command::SetMuted { muted: false })?;
    Ok(measured)
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

/// Plays `samples` (mono, 48 kHz) into `client`'s microphone, in the background: a raw file
/// written to its run directory, which `pw-play` plays into the sink that feeds it.
fn play_into_microphone(the_box: &TestBox, client: &'static str, samples: &[f32]) -> Result<()> {
    let file = "microphone.raw";
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
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
