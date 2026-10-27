//! `cargo testbox test <phase>`'s checks. Each phase is a file of its own.

mod crashed;
mod e2ee;
mod impersonation;
mod record;
mod relay;
mod roster;
mod testbox;

use crate::error::{Error, Result};
use crate::podman::TestBox;

pub use self::record::Record;

/// The phases, each a file of its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Phase {
    /// The box itself: no egress, no build when nothing changed, the builder, displays, the GPU.
    Box,
    /// Frames pass through the relay between a stock moq publisher and subscriber.
    Relay,
    /// Participants see each other come and go, and rejoin a restarted relay.
    Roster,
    /// A stranger on the relay sees only opaque names and ciphertext.
    E2ee,
    /// A member publishing what it can't sign is rejected.
    Impersonation,
}

impl Phase {
    pub const ALL: [Phase; 5] = [
        Phase::Box,
        Phase::Relay,
        Phase::Roster,
        Phase::E2ee,
        Phase::Impersonation,
    ];

    pub fn name(self) -> String {
        clap::ValueEnum::to_possible_value(&self)
            .map(|value| value.get_name().to_owned())
            .unwrap_or_default()
    }

    /// Whether it drives the box's clients: such phases run one at a time, even with `--jobs`,
    /// as two driving alice at once would take each other's rooms. (Leasing clients to phases
    /// lifts that, once there are more phases with clients.)
    pub fn uses_clients(self) -> bool {
        matches!(self, Phase::Roster | Phase::E2ee | Phase::Impersonation)
    }

    /// Whether it disrupts the whole box (`roster` restarts the relay): such a phase runs alone,
    /// with no other phase at once.
    pub fn runs_alone(self) -> bool {
        matches!(self, Phase::Roster)
    }

    pub fn run(self, checks: &mut Checks, the_box: &TestBox) -> Result<()> {
        match self {
            Phase::Box => testbox::testbox(checks, the_box),
            Phase::Relay => relay::relay(checks, the_box),
            Phase::Roster => roster::roster(checks, the_box),
            Phase::E2ee => e2ee::e2ee(checks, the_box),
            Phase::Impersonation => impersonation::impersonation(checks, the_box),
        }
    }
}

/// Runs a phase's checks on a fresh tally, then fails if any of them failed. Last, also after a
/// check the rest depend on failed: that no container crashed ([`crashed`]); then the logs go
/// into the phase's [`Record`].
pub fn phase(
    the_box: &TestBox,
    record: Record,
    prefix: &str,
    run: impl FnOnce(&mut Checks) -> Result<()>,
) -> Result<()> {
    let mut checks = Checks {
        the_box,
        failed: Vec::new(),
        kept: record,
        prefix: prefix.to_owned(),
    };
    let ran = run(&mut checks);
    checks.record("no container crashed", || crashed::none(the_box));
    checks.kept.keep_logs(the_box);
    println!(
        "{}record: {}",
        checks.prefix,
        checks.kept.directory().display()
    );
    ran?;
    checks.finish()
}

pub struct Checks<'a> {
    the_box: &'a TestBox,
    failed: Vec<String>,
    kept: Record,
    /// What each line of output starts with: the phase's name, when phases run at once.
    prefix: String,
}

impl Checks<'_> {
    /// A check later checks depend on: its failure ends the run (the caller's `?`).
    pub fn require<Found>(
        &mut self,
        name: &str,
        check: impl FnOnce() -> Result<Found>,
    ) -> Result<Found> {
        self.report(name, check())
            .ok_or_else(|| Error::Failed(format!("{name} failed")))
    }

    /// A check nothing depends on: a failure is tallied for [`Self::finish`] and the run goes on.
    pub fn record(&mut self, name: &str, check: impl FnOnce() -> Result<()>) {
        self.report(name, check());
    }

    /// A check that doesn't apply to this box, and why: said, not tallied.
    pub fn skip(&mut self, name: &str, why: &str) {
        println!("{}check {name}: skipped: {why}", self.prefix);
    }

    /// Where the phase's record is, for a check that keeps a file there.
    pub fn record_directory(&self) -> &std::path::Path {
        self.kept.directory()
    }

    /// Prints the outcome; a failure is tallied and yields `None`.
    fn report<Found>(&mut self, name: &str, outcome: Result<Found>) -> Option<Found> {
        self.kept.checked(self.the_box, name, outcome.is_err());
        match outcome {
            Ok(found) => {
                println!("{}check {name}: passed", self.prefix);
                Some(found)
            }
            Err(reason) => {
                println!("{}check {name}: failed: {reason}", self.prefix);
                self.failed.push(name.to_owned());
                None
            }
        }
    }

    fn finish(self) -> Result<()> {
        if self.failed.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(format!("failed: {}", self.failed.join(", "))))
        }
    }
}
