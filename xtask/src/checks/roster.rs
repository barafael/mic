//! The `roster` phase: alice and bob join a room of their own and see each other, a new name
//! reaches the other, bob leaves and alice sees him go, and a restarted relay is rejoined, all
//! through their harnesses.

use std::time::Duration;

use mic_core::{Command, Connection, Invite};

use crate::error::Result;
use crate::harness::{Participant, names};
use crate::podman::{Pod, TestBox};
use crate::{process, ready};

use super::Checks;

/// How long a roster may take to show a change.
const PATIENCE: Duration = Duration::from_secs(15);

pub fn roster(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let (mut alice, mut bob) = checks.require("alice and bob answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
        ))
    })?;
    // A room of its own: no former run's or concurrent phase's participants in it.
    let invite = Invite::new_room();
    checks.require("alice and bob see each other", || {
        alice.join(&invite, "Alice")?;
        bob.join(&invite, "Bob")?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        bob.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        Ok(())
    })?;
    checks.record("a new name reaches the other", || {
        alice.act(Command::SetName {
            name: "Alice B.".to_owned(),
        })?;
        bob.wait_for("with Alice B.", PATIENCE, |state| {
            names(state) == ["Alice B."]
        })?;
        Ok(())
    });
    checks.record("alice sees bob go", || {
        bob.reset()?;
        alice.wait_for("alone", PATIENCE, |state| state.participants.is_empty())?;
        Ok(())
    });
    checks.record("a restarted relay is rejoined", || {
        bob.join(&invite, "Bob")?;
        alice.wait_for("with Bob", PATIENCE, |state| names(state) == ["Bob"])?;
        process::run(crate::podman::podman(&[
            "restart",
            "--time",
            "1",
            &Pod::Relay.container("relay"),
        ]))?;
        ready::relay(the_box)?;
        for participant in [&mut alice, &mut bob] {
            participant.wait_for("connected again", PATIENCE, |state| {
                state.connection == Connection::Connected
            })?;
        }
        alice.wait_for("with Bob, once", PATIENCE, |state| names(state) == ["Bob"])?;
        bob.wait_for("with Alice B., once", PATIENCE, |state| {
            names(state) == ["Alice B."]
        })?;
        Ok(())
    });
    checks.record("an honest room rejects nobody", || {
        for participant in [&mut alice, &mut bob] {
            participant.wait_for("rejecting nobody", Duration::ZERO, |state| {
                state.rejected.is_empty()
            })?;
        }
        Ok(())
    });
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    Ok(())
}
