//! The `video` phase: alice and bob send pattern cameras (`podman::camera`), carol claims to
//! decode no video (`--decoders none`). Each of alice and bob decodes the other's camera, the
//! pattern names the right sender, and the frame rate holds; carol decodes nothing, and her window
//! says so on each tile rather than showing black. It measures a frame rate, so it runs alone.

use std::time::{Duration, Instant};

use mic_core::Invite;

use crate::error::{Error, Result};
use crate::harness::{Participant, names};
use crate::podman::{TestBox, camera};

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
