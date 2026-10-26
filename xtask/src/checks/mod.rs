//! `cargo testbox test <phase>`'s checks. Each phase is a file of its own.

mod crashed;
mod record;
mod relay;
mod testbox;

use crate::error::{Error, Result};
use crate::podman::TestBox;

pub use self::record::Record;
pub use self::relay::relay;
pub use self::testbox::testbox;

/// Runs a phase's checks on a fresh tally, then fails if any of them failed. Last, also after a
/// check the rest depend on failed: that no container crashed ([`crashed`]); then the logs go
/// into the phase's [`Record`].
pub fn phase(
    the_box: &TestBox,
    record: &Record,
    run: impl FnOnce(&mut Checks) -> Result<()>,
) -> Result<()> {
    let mut checks = Checks { failed: Vec::new() };
    let ran = run(&mut checks);
    checks.record("no container crashed", || crashed::none(the_box));
    record.keep_logs(the_box);
    ran?;
    checks.finish()
}

pub struct Checks {
    failed: Vec<String>,
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

    /// Prints the outcome; a failure is tallied and yields `None`.
    fn report<Found>(&mut self, name: &str, outcome: Result<Found>) -> Option<Found> {
        match outcome {
            Ok(found) => {
                println!("check {name}: passed");
                Some(found)
            }
            Err(reason) => {
                println!("check {name}: failed: {reason}");
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
