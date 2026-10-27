//! The `e2ee` phase: what a stranger on the relay sees of a room, through the relay's own HTTP
//! interface (what it announces, the latest group of a track), asked from the tools pod: opaque
//! names and ciphertext, nothing of the room, its participants or what they say (D17). And a
//! stranger who learned the room's prefix, reading its tracks by their real names, can't open
//! them without the room's secret (the forger's `eavesdrop`, testbox/forger).

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
const RELAY: &str = "http://mic-testbox-relay:4080";

pub fn e2ee(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut bob) = checks.require("alice and bob answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
        ))
    })?;
    let invite = Invite::new_room();
    let crypto = RoomCrypto::new(&invite).map_err(|error| Error::Failed(error.to_string()))?;
    // Names of this run's own, long enough that no opaque path or ciphertext holds one by chance,
    // as it would "Bob" now and then.
    let tag = &invite.room.to_string()[..12];
    let (alice_name, bob_name) = (format!("Alice {tag}"), format!("Bob {tag}"));
    let (alice_key, bob_key) = checks.require("alice and bob are in a room", || {
        let alice_key = alice.join(&invite, &alice_name)?.me.key;
        let bob_key = bob.join(&invite, &bob_name)?.me.key;
        alice.wait_for("with Bob", PATIENCE, |state| {
            names(state) == [bob_name.as_str()]
        })?;
        Ok((alice_key, bob_key))
    })?;
    // What must appear nowhere on the relay. The room's id is the tag's source, so it's
    // searched for whole, not the tag alone.
    let secrets = [
        invite.room.to_string(),
        alice_key.to_string(),
        bob_key.to_string(),
        alice_name.clone(),
        bob_name.clone(),
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
    checks.record("under the room's own secret, its profiles open", || {
        let lines = eavesdrop(the_box, &invite, &invite)?;
        for name in [&alice_name, &bob_name] {
            if !lines
                .iter()
                .any(|line| line.starts_with("read ") && line.contains(name.as_str()))
            {
                return Err(Error::Failed(format!(
                    "{name}'s profile wasn't read: {lines:?}"
                )));
            }
        }
        Ok(())
    });
    checks.record("without the room's secret, its profiles don't open", || {
        let forged = Invite {
            room: invite.room,
            secret: RoomSecret::random(),
        };
        let lines = eavesdrop(the_box, &invite, &forged)?;
        // Refused for not opening (moq-e2ee's `Authentication`), not for anything else, such as
        // a profile that never came.
        if lines
            .iter()
            .all(|line| line.starts_with("refused ") && line.ends_with(": authentication"))
        {
            Ok(())
        } else {
            Err(Error::Failed(format!("a profile opened: {lines:?}")))
        }
    });
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    Ok(())
}

/// The forger in the tools pod, eavesdropping on the two broadcasts of the room `invite` names,
/// decrypting under `with`'s secret: what it said of each.
fn eavesdrop(the_box: &TestBox, invite: &Invite, with: &Invite) -> Result<Vec<String>> {
    let ran = the_box.exec(
        &Pod::Tools.container("tools"),
        &[
            "/opt/mic/forger",
            "--relay",
            "https://mic-testbox-relay:4443/",
            "--ca",
            "/keys/ca.pem",
            "--invite",
            &invite.to_mic_url(),
            "eavesdrop",
            "--with",
            &with.to_mic_url(),
            "--broadcasts",
            "2",
        ],
    )?;
    let said = String::from_utf8_lossy(&ran.stdout);
    let lines: Vec<String> = said
        .lines()
        .filter(|line| line.starts_with("read ") || line.starts_with("refused "))
        .map(str::to_owned)
        .collect();
    if ran.status.success() && lines.len() == 2 {
        Ok(lines)
    } else {
        Err(Error::Failed(format!(
            "the forger said {said:?}: {}",
            String::from_utf8_lossy(&ran.stderr).trim()
        )))
    }
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
