//! The `audio` phase: alice and bob, each sending a tone of their own (`podman::tone`), hear each
//! other's and never their own, through the relay, encrypted and signed; and once bob leaves,
//! alice hears him no more. It measures what the clients' analyzers heard, so it runs alone
//! (`Phase::runs_alone`): another phase's load would make the audio it measures stutter.

use std::time::Duration;

use mic_core::Invite;

use crate::error::Result;
use crate::harness::{Participant, names};
use crate::podman::{TestBox, tone};

use super::Checks;

/// How long a roster, or what's heard, may take to show a change.
const PATIENCE: Duration = Duration::from_secs(15);

pub fn audio(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut bob) = checks.require("alice and bob answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
        ))
    })?;
    let (alices, bobs) = (f64::from(tone("alice")), f64::from(tone("bob")));
    let invite = Invite::new_room();
    checks.require("alice and bob are in a room", || {
        alice.join(&invite, "Alice")?;
        bob.join(&invite, "Bob")?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        bob.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        Ok(())
    })?;
    checks.record("each hears the other's tone, never their own", || {
        alice.wait_to_hear(bobs, &[alices], PATIENCE)?;
        bob.wait_to_hear(alices, &[bobs], PATIENCE)?;
        Ok(())
    });
    checks.record("once bob leaves, alice hears him no more", || {
        bob.reset()?;
        alice.wait_for("alone", PATIENCE, |state| state.participants.is_empty())?;
        alice.wait_for_silence(&[bobs], PATIENCE)
    });
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    Ok(())
}
