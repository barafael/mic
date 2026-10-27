//! The `audio` phase: alice and bob, each sending a tone of their own (`podman::tone`), hear each
//! other's and never their own, through the relay, encrypted and signed; muted, alice is silence
//! on bob's end and her tile says so, and unmuted she's heard again; and once bob leaves, alice
//! hears him no more. It measures what the clients' analyzers heard, so it runs alone
//! (`Phase::runs_alone`): another phase's load would make the audio it measures stutter.

use std::time::Duration;

use mic_core::{Command, Invite};

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
    let alice_key = checks.require("alice and bob are in a room", || {
        let key = alice.join(&invite, "Alice")?.me.key.to_string();
        bob.join(&invite, "Bob")?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        bob.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        Ok(key)
    })?;
    checks.record("each hears the other's tone, never their own", || {
        alice.wait_to_hear(bobs, &[alices], PATIENCE)?;
        bob.wait_to_hear(alices, &[bobs], PATIENCE)?;
        Ok(())
    });
    checks.record(
        "muted, alice is silence on bob's end, and her tile says so",
        || {
            alice.act(Command::SetMuted { muted: true })?;
            bob.wait_for_silence(&[alices], PATIENCE)?;
            bob.wait_for_snapshot("showing alice muted", PATIENCE, |snapshot| {
                snapshot
                    .ui
                    .as_ref()
                    .is_some_and(|ui| ui.muted.contains(&alice_key))
            })?;
            // Muting is hers alone.
            alice.wait_to_hear(bobs, &[alices], PATIENCE)?;
            alice.act(Command::SetMuted { muted: false })?;
            bob.wait_to_hear(alices, &[bobs], PATIENCE)?;
            bob.wait_for_snapshot("showing alice unmuted", PATIENCE, |snapshot| {
                snapshot.ui.as_ref().is_some_and(|ui| ui.muted.is_empty())
            })?;
            Ok(())
        },
    );
    checks.record("bob mutes and unmutes with his window's button", || {
        super::window::click_mute(the_box, "bob")?;
        alice.wait_for("with Bob muted", PATIENCE, |state| {
            state.participants.iter().any(|them| them.profile.muted)
        })?;
        alice.wait_for_silence(&[bobs], PATIENCE)?;
        super::window::click_mute(the_box, "bob")?;
        alice.wait_for("with Bob unmuted", PATIENCE, |state| {
            state.participants.iter().all(|them| !them.profile.muted)
        })?;
        alice.wait_to_hear(bobs, &[alices], PATIENCE)?;
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
