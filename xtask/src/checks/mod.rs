//! `cargo testbox test <phase>`'s checks. Each phase is a file of its own.

mod crashed;
mod record;
mod relay;
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
}

impl Phase {
    pub const ALL: [Phase; 2] = [Phase::Box, Phase::Relay];

    pub fn name(self) -> String {
        clap::ValueEnum::to_possible_value(&self)
            .map(|value| value.get_name().to_owned())
            .unwrap_or_default()
    }

    pub fn run(self, checks: &mut Checks, the_box: &TestBox) -> Result<()> {
        match self {
            Phase::Box => testbox::testbox(checks, the_box),
            Phase::Relay => relay::relay(checks, the_box),
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

pub struct Checks {
    failed: Vec<String>,
    kept: Record,
    /// What each line of output starts with: the phase's name, when phases run at once.
    prefix: String,
}

impl Checks {
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
        self.kept.shoot(name);
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
