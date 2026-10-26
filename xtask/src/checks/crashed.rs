//! The last check of every phase: no container of the box restarted, stopped with a failure, or
//! isn't running. A pod restarts what exits (`restartPolicy: Always`), so a crash shows as a
//! restart; one that stopped cleanly, but stopped, is no more there for the next phase. Init
//! containers (the relay's `keys`) run once and exit.

use crate::error::{Error, Result};
use crate::podman::TestBox;

/// What the box's init containers' names end with, which run once and exit.
const INIT: &str = "-keys";

pub fn none(the_box: &TestBox) -> Result<()> {
    let mut crashed = Vec::new();
    let containers = the_box.containers();
    if containers.is_empty() {
        return Err(Error::Failed("the box has no containers".to_owned()));
    }
    for container in containers {
        let (restarts, code) = the_box.restarts(&container)?;
        if restarts > 0 || code != 0 {
            crashed.push(format!(
                "{container} (restarted {restarts} times, exit code {code})"
            ));
        } else if !container.ends_with(INIT) && !the_box.running(&container) {
            crashed.push(format!("{container} (not running)"));
        }
    }
    if crashed.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(crashed.join(", ")))
    }
}
