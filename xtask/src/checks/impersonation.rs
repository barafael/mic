//! The `impersonation` phase (D18): a member of the room, the box's forger (testbox/forger), run
//! in the tools pod with the room's invite, publishes what it can't sign. At Alice's path with a
//! catalog it signed itself, it must stand for no one; at its own path with frames no checkpoint
//! covers, it must be rejected within the grace. Bob sees Alice once, and lists both as rejected.
//! The forger speaks, in a tone apart (`FORGED`): once it's rejected, bob hears only alice.

use std::time::{Duration, Instant};

use mic_core::Invite;
use mic_core::sig::GRACE;

use crate::error::{Error, Result};
use crate::harness::{Participant, names};
use crate::podman::{Pod, TestBox};

use super::Checks;

/// How long a roster may take to show a change.
const PATIENCE: Duration = Duration::from_secs(15);
/// The forger's voice (testbox/forger, `FORGED_TONE`).
const FORGED: f64 = 880.0;
/// How long a forger publishes: longer than the checks wait for it.
const FORGING_SECS: u64 = 20;

pub fn impersonation(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut bob) = checks.require("alice and bob answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
        ))
    })?;
    let alices = f64::from(crate::podman::tone("alice"));
    let invite = Invite::new_room();
    let alice_key = checks.require("alice and bob are in a room", || {
        let key = alice.join(&invite, "Alice")?.me.key;
        bob.join(&invite, "Bob")?;
        bob.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        Ok(key)
    })?;
    checks.record("a forger at alice's path stands for no one", || {
        forge(the_box, &invite, &["at-path", &alice_key.to_string()])?;
        bob.wait_for("rejecting the forgery", PATIENCE, |state| {
            state
                .rejected
                .iter()
                .any(|rejected| rejected.reason.contains("whose path this isn't"))
        })?;
        bob.wait_for("with Alice, once", Duration::ZERO, |state| {
            state.participants.len() == 1 && state.participants[0].key == alice_key
        })?;
        bob.wait_to_hear(alices, &[FORGED], PATIENCE)?;
        Ok(())
    });
    checks.record(
        "frames no signed checkpoint covers are rejected within the grace",
        || {
            let started = Instant::now();
            forge(the_box, &invite, &["unsigned"])?;
            // Frames play as they come and are confirmed after (D18), so bob hears the forger
            // until he rejects it: the voice took the path a signed one would.
            let mut loudest: f64 = 0.0;
            loop {
                loudest = loudest.max(bob.heard(&[FORGED])?.levels[0]);
                let state = bob.wait_for("in the room", Duration::ZERO, |_| true)?;
                if state
                    .rejected
                    .iter()
                    .any(|rejected| rejected.reason.contains("no signed checkpoint covered"))
                {
                    break;
                }
                if started.elapsed() > PATIENCE {
                    return Err(Error::Failed(format!(
                        "the unsigned frames weren't rejected within {}s",
                        PATIENCE.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if loudest < crate::harness::SILENT * 5.0 {
                return Err(Error::Failed(format!(
                    "bob never heard the forger before rejecting it ({FORGED} Hz at {loudest}): \
                     its voice wasn't played at all"
                )));
            }
            let took = started.elapsed();
            // The grace, and what the forger takes to start and the check to notice.
            if took > GRACE + Duration::from_secs(3) {
                return Err(Error::Failed(format!(
                    "rejected after {:.1}s, the grace is {}s",
                    took.as_secs_f64(),
                    GRACE.as_secs()
                )));
            }
            bob.wait_for("with Alice, once", Duration::ZERO, |state| {
                names(state) == ["Alice"]
            })?;
            // What it said before it was rejected plays out of the analyzer's second.
            bob.wait_to_hear(alices, &[FORGED], PATIENCE)?;
            Ok(())
        },
    );
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    Ok(())
}

/// The forger, in the tools pod, publishing in the room `invite` names for [`FORGING_SECS`], in
/// the background.
fn forge(the_box: &TestBox, invite: &Invite, forgery: &[&str]) -> Result<()> {
    let command = format!(
        "timeout {FORGING_SECS} /opt/mic/forger --relay https://mic-testbox-relay:4443/ \
         --ca /keys/ca.pem --invite '{}' {} >/dev/null 2>&1 &",
        invite.to_mic_url(),
        forgery.join(" ")
    );
    let started = the_box.exec(&Pod::Tools.container("tools"), &["bash", "-c", &command])?;
    if started.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "the forger didn't start: {}",
            String::from_utf8_lossy(&started.stderr)
        )))
    }
}
