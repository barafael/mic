//! The last check of every phase: no container of the box restarted, or stopped with a failure.
//! A pod restarts what exits (`restartPolicy: Always`), so a crash shows as a restart.

use crate::error::{Error, Result};
use crate::podman::TestBox;

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
        }
    }
    if crashed.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(crashed.join(", ")))
    }
}
