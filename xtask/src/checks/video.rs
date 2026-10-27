//! The `video` phase: alice and bob send pattern cameras (`podman::camera`), carol claims to
//! decode no video (`--decoders none`). Each of alice and bob decodes the other's camera, the
//! pattern names the right sender, and the frame rate holds; their windows show it, in the
//! sender's colour, and carol's says on each tile that it can't, rather than show black. Then a
//! crowd of six with cameras of their own fills alice's window to nine tiles, and what drawing
//! them costs is measured and kept in the record (`render.txt`), as the spike measured it. It
//! measures, so it runs alone.

use std::path::Path;
use std::time::{Duration, Instant};

use mic_core::Invite;

use crate::error::{Error, Result};
use crate::harness::{Participant, names};
use crate::podman::{Pod, TestBox, camera};
use crate::view;

use super::Checks;

/// How long a roster, or a camera, may take to show.
const PATIENCE: Duration = Duration::from_secs(15);
/// The camera's frame rate (mic-core's `CAMERA_SETTINGS`), and what of it must arrive.
const FPS: f64 = 15.0;
const RATE_HELD: f64 = 0.7;

pub fn video(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut bob, mut carol) = checks.require("alice, bob and carol answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
            Participant::connect(the_box, "carol")?,
        ))
    })?;
    let invite = Invite::new_room();
    let (alice_key, bob_key) = checks.require("they're in a room", || {
        let alice_key = alice.join(&invite, "Alice")?.me.key.to_string();
        let bob_key = bob.join(&invite, "Bob")?.me.key.to_string();
        carol.join(&invite, "Carol")?;
        for participant in [&mut alice, &mut bob, &mut carol] {
            participant.wait_for("with the others", PATIENCE, |state| names(state).len() == 2)?;
        }
        Ok((alice_key, bob_key))
    })?;
    let alices = camera("alice").ok_or("alice sends no camera")?;
    let bobs = camera("bob").ok_or("bob sends no camera")?;
    checks.record("each decodes the other's camera, and it's theirs", || {
        sees(&mut alice, &bob_key, bobs)?;
        sees(&mut bob, &alice_key, alices)
    });
    checks.record("the frame rate holds", || {
        for (viewer, whose) in [(&mut alice, &bob_key), (&mut bob, &alice_key)] {
            let rate = rate(viewer, whose)?;
            if rate < FPS * RATE_HELD {
                return Err(Error::Failed(format!(
                    "{}: {rate:.1} pictures a second of {whose}'s camera, of {FPS}",
                    viewer.name
                )));
            }
        }
        Ok(())
    });
    let directory = checks.record_directory().to_owned();
    checks.record("their windows show the other's camera, in its colour", || {
        let tool = view::tool(the_box.repo(), "testbox-shot")?;
        for (client, shows, not) in [
            ("alice", &[bobs][..], &[alices][..]),
            ("bob", &[alices][..], &[bobs][..]),
            ("carol", &[][..], &[alices, bobs][..]),
        ] {
            let deadline = Instant::now() + PATIENCE;
            loop {
                let counts = colours(the_box, &tool, &directory, client, &[alices, bobs])?;
                let count = |sender: u8| counts[usize::from(sender == bobs)];
                let right = shows.iter().all(|&sender| count(sender) >= SHOWN)
                    && not.iter().all(|&sender| count(sender) < SHOWN / 10);
                if right {
                    break;
                }
                if Instant::now() > deadline {
                    return Err(Error::Failed(format!(
                        "{client}'s display: camera {alices}'s colour in {} pixels, {bobs}'s in {}",
                        count(alices),
                        count(bobs)
                    )));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        Ok(())
    });
    checks.record("nine tiles: what drawing alice's costs", || {
        crowd(the_box, &invite)?;
        alice.wait_for_snapshot(
            "showing nine tiles, seven with video",
            PATIENCE,
            |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| {
                    ui.tiles.len() == 9
                        && ui
                            .render
                            .as_ref()
                            .is_some_and(|render| render.video_tiles == 7)
                })
            },
        )?;
        let mut lines = Vec::new();
        let mut pictures = 0.0;
        let (cpu_before, started) = (cpu_seconds(the_box, "alice")?, Instant::now());
        for _ in 0..COST_SAMPLES {
            std::thread::sleep(Duration::from_secs(1));
            let render = alice
                .snapshot()?
                .ui
                .and_then(|ui| ui.render)
                .ok_or("alice's window says nothing of its cost")?;
            pictures += render.pictures / COST_SAMPLES as f64;
            lines.push(format!(
                "{} video tiles: {:.1} pictures/s, draw {:.2} ms each, frame {}",
                render.video_tiles,
                render.pictures,
                render.draw_ms,
                render
                    .frame_ms
                    .map_or("unknown".to_owned(), |ms| format!("{ms:.2} ms")),
            ));
        }
        let cpu = (cpu_seconds(the_box, "alice")? - cpu_before) / started.elapsed().as_secs_f64();
        lines.push(format!(
            "alice's mic, decoding 7 cameras and drawing 9 tiles: {:.0}% of a core",
            cpu * 100.0
        ));
        let report = lines.join("\n") + "\n";
        print!("{report}");
        crate::error::write(&directory.join("render.txt"), &report)?;
        let wanted = 7.0 * FPS * RATE_HELD;
        if pictures < wanted {
            return Err(Error::Failed(format!(
                "{pictures:.1} pictures a second drawn, of {wanted:.0} wanted"
            )));
        }
        Ok(())
    });
    checks.record("carol, who decodes nothing, says so on each tile", || {
        carol.wait_for_snapshot("saying she can't show their video", PATIENCE, |snapshot| {
            snapshot.ui.as_ref().is_some_and(|ui| {
                ui.cant_decode.contains(&alice_key) && ui.cant_decode.contains(&bob_key)
            })
        })?;
        let decoded: u64 = carol
            .seen()?
            .cameras
            .iter()
            .map(|camera| camera.pictures)
            .sum();
        if decoded == 0 {
            Ok(())
        } else {
            Err(Error::Failed(format!("carol decoded {decoded} pictures")))
        }
    });
    for participant in [&mut alice, &mut bob, &mut carol] {
        participant.reset()?;
    }
    Ok(())
}

/// How many pixels of a sender's colour a tile showing its camera has, at least, on a display:
/// a sixteenth of a tile's area, which a tile's background alone fills many times over.
const SHOWN: usize = 8_000;
/// How many times the nine-tile cost is sampled, a second apart.
const COST_SAMPLES: usize = 5;
/// What a colour on the display may be off by, per channel: Slint's sampling, and the codec.
const TOLERANCE: u8 = 12;

/// How many pixels of each of `senders`' colours `client`'s display has, as a screenshot kept in
/// the record shows it.
fn colours(
    the_box: &TestBox,
    tool: &Path,
    directory: &Path,
    client: &'static str,
    senders: &[u8],
) -> Result<Vec<usize>> {
    let display = format!("{client}={}", the_box.view_socket(client).display());
    view::shot(tool, directory, "video", std::slice::from_ref(&display))?;
    let (pixels, channels) =
        crate::checks::png_pixels(&directory.join(format!("video-{client}.png")))?;
    Ok(senders
        .iter()
        .map(|&sender| {
            let colour = colour(sender);
            pixels
                .chunks_exact(channels)
                .filter(|pixel| {
                    pixel[..3]
                        .iter()
                        .zip(&colour)
                        .all(|(a, b)| a.abs_diff(*b) <= TOLERANCE)
                })
                .count()
        })
        .collect())
}

/// The CPU time `client`'s mic process has used, user and system, in seconds (`/proc`, in
/// clock ticks of a hundredth of a second, as Linux has them).
fn cpu_seconds(the_box: &TestBox, client: &'static str) -> Result<f64> {
    let read = the_box.exec(
        &view::container(client),
        &["sh", "-c", "cat /proc/$(pgrep -x mic)/stat"],
    )?;
    let stat = String::from_utf8_lossy(&read.stdout);
    // After the command's name in parentheses: utime and stime are the 12th and 13th fields.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    let ticks = |at: usize| -> Result<f64> {
        fields
            .get(at)
            .and_then(|field| field.parse::<f64>().ok())
            .ok_or_else(|| Error::Failed(format!("{client}'s mic's /proc stat: {stat:?}")))
    };
    Ok((ticks(11)? + ticks(12)?) / 100.0)
}

/// A sender's pattern's background colour, as RGB: its luma and tint, as mic-render shows them.
fn colour(sender: u8) -> [u8; 3] {
    let (u, v) = mic_media::video::tint(sender);
    let mut picture = mic_media::video::Picture::grey(2, 2);
    picture.y_mut().fill(mic_media::video::BACKGROUND);
    let (us, vs) = picture.uv_mut();
    us.fill(u);
    vs.fill(v);
    let rgba = mic_render::rgba(&picture);
    [rgba[0], rgba[1], rgba[2]]
}

/// Six crowd participants with cameras of their own (patterns 11 to 16) in the room `invite`
/// names, from the tools pod, for long enough to measure, decoding nothing themselves.
fn crowd(the_box: &TestBox, invite: &Invite) -> Result<()> {
    let names: Vec<String> = (1..=6).map(|i| format!("crowd-{i}@{}", 10 + i)).collect();
    let command = format!(
        "mkdir -p /tmp/crowd && timeout 40 /opt/mic/mic-crowd \
         --relay https://mic-testbox-relay:4443/ --ca /keys/ca.pem --harness /tmp/crowd \
         --no-decoders --join '{}' {} >/tmp/crowd/log 2>&1 &",
        invite.to_mic_url(),
        names.join(" ")
    );
    let started = the_box.exec(&Pod::Tools.container("tools"), &["bash", "-c", &command])?;
    if started.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "the crowd didn't start: {}",
            String::from_utf8_lossy(&started.stderr)
        )))
    }
}

/// Waits until `viewer` decodes `whose` camera with `sender`'s pattern.
pub fn sees(viewer: &mut Participant, whose: &str, sender: u8) -> Result<()> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let seen = viewer.seen()?;
        let camera = seen.cameras.iter().find(|camera| camera.key == whose);
        if camera.is_some_and(|camera| camera.sender == Some(sender)) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "{}: no picture of camera {sender} within {}s: {camera:?}",
                viewer.name,
                PATIENCE.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// How many pictures of `whose` camera `viewer` decodes a second, over three seconds.
fn rate(viewer: &mut Participant, whose: &str) -> Result<f64> {
    let pictures = |viewer: &mut Participant| -> Result<u64> {
        Ok(viewer
            .seen()?
            .cameras
            .iter()
            .find(|camera| camera.key == whose)
            .map_or(0, |camera| camera.pictures))
    };
    let (before, started) = (pictures(viewer)?, Instant::now());
    std::thread::sleep(Duration::from_secs(3));
    let after = pictures(viewer)?;
    Ok((after - before) as f64 / started.elapsed().as_secs_f64())
}
