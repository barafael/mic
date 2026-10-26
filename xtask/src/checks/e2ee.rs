//! The `e2ee` phase: what a stranger on the relay sees of a room, through the relay's own HTTP
//! interface (what it announces, the latest group of a track), asked from the tools pod: opaque
//! names and ciphertext, nothing of the room, its participants or what they say (D17). And a
//! participant without the room's secret finds nobody.

use std::time::Duration;

use mic_core::tracks::RoomCrypto;
use mic_core::{Invite, RoomSecret};

use crate::error::{Error, Result};
use crate::harness::{Participant, names};
use crate::podman::{Pod, TestBox};

use super::Checks;

/// How long a roster may take to show a change.
const PATIENCE: Duration = Duration::from_secs(15);
/// The relay's HTTP interface, as the pods reach it.
const RELAY: &str = "http://mic-testbox-relay:4443";

pub fn e2ee(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let mut alice = Participant::connect(the_box, "alice")?;
    let mut bob = Participant::connect(the_box, "bob")?;
    let invite = Invite::new_room();
    let crypto = RoomCrypto::new(&invite).map_err(|error| Error::Failed(error.to_string()))?;
    let (alice_key, bob_key) = checks.require("alice and bob are in a room", || {
        let alice_key = alice.join(&invite, "Alice")?.me.key;
        let bob_key = bob.join(&invite, "Bob")?.me.key;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        Ok((alice_key, bob_key))
    })?;
    // What must appear nowhere on the relay.
    let secrets = [
        invite.room.to_string(),
        alice_key.to_string(),
        bob_key.to_string(),
        "Alice".to_owned(),
        "Bob".to_owned(),
    ];
    let paths = checks.require("the relay names neither room nor participants", || {
        let announced = String::from_utf8_lossy(&fetch(the_box, "/announced")?).into_owned();
        let prefix = format!("{}/", crypto.prefix().as_str());
        let paths: Vec<String> = announced
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix))
            .map(str::to_owned)
            .collect();
        if paths.len() != 2 {
            return Err(Error::Failed(format!(
                "{} broadcasts under the room's prefix, not 2: {announced:?}",
                paths.len()
            )));
        }
        let named: Vec<&String> = secrets
            .iter()
            .filter(|secret| announced.contains(secret.as_str()))
            .collect();
        if named.is_empty() {
            Ok(paths)
        } else {
            Err(Error::Failed(format!(
                "the relay's announcements name {named:?}"
            )))
        }
    })?;
    checks.record("the relay carries only ciphertext", || {
        for path in &paths {
            let generation = crypto
                .generation_of(path)
                .map_err(|error| Error::Failed(error.to_string()))?;
            let track = generation
                .name(mic_core::roster::PROFILE)
                .map_err(|error| Error::Failed(error.to_string()))?;
            let bytes = fetch(
                the_box,
                &format!("/fetch/{}/{path}/{track}", crypto.prefix().as_str()),
            )?;
            if bytes.is_empty() {
                return Err(Error::Failed(format!("{path}'s profile is empty")));
            }
            let found: Vec<&String> = secrets
                .iter()
                .filter(|secret| {
                    bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                })
                .collect();
            if !found.is_empty() {
                return Err(Error::Failed(format!("{path}'s profile carries {found:?}")));
            }
        }
        Ok(())
    });
    checks.record(
        "a participant without the room's secret finds nobody",
        || {
            bob.reset()?;
            alice.wait_for("alone", PATIENCE, |state| state.participants.is_empty())?;
            let forged = Invite {
                room: invite.room,
                secret: RoomSecret::random(),
            };
            bob.join(&forged, "Bob")?;
            // Long enough for a roster to fill, were there anyone to find.
            std::thread::sleep(Duration::from_secs(3));
            for participant in [&mut alice, &mut bob] {
                participant.wait_for("alone", Duration::ZERO, |state| {
                    state.participants.is_empty()
                })?;
            }
            Ok(())
        },
    );
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    Ok(())
}

/// `path` of the relay's HTTP interface, asked from the tools pod, as anyone on the relay's
/// network could.
fn fetch(the_box: &TestBox, path: &str) -> Result<Vec<u8>> {
    let url = format!("{RELAY}{path}");
    let fetched = the_box.exec(
        &Pod::Tools.container("tools"),
        &["curl", "--silent", "--fail", "--max-time", "10", &url],
    )?;
    if fetched.status.success() {
        Ok(fetched.stdout)
    } else {
        Err(Error::Failed(format!(
            "{url}: {}",
            String::from_utf8_lossy(&fetched.stderr).trim()
        )))
    }
}
