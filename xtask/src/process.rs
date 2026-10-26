//! Running the programs the xtask drives: to success, or failing with what they printed.

use std::process::{Command, Stdio};

use crate::error::{Context, Error, Result};

/// `program args…`, as an error names a command.
pub fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs with the terminal attached; fails on a non-zero exit.
pub fn run(mut command: Command) -> Result<()> {
    let status = command.status().context(describe(&command))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Command {
            command: describe(&command),
            output: format!(" ({status})"),
        })
    }
}

/// Runs without input; returns stdout, and fails with stdout and stderr on a non-zero exit.
pub fn output(mut command: Command) -> Result<String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .context(describe(&command))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(Error::Command {
            command: describe(&command),
            output: format!(
                " ({})\n{stdout}{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        })
    }
}

/// Runs without input, and says only whether it succeeded: for questions podman answers with its
/// exit status (`image exists`).
pub fn succeeds(mut command: Command) -> Result<bool> {
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context(describe(&command))?;
    Ok(status.success())
}

/// A name no other run of the box shares: nanoseconds since the epoch, and the process. For rooms
/// and broadcasts, so a former run's or a concurrent phase's never answers for a check's.
pub fn unique(what: &str) -> Result<String> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_err(|error| Error::Failed(format!("the clock is before 1970: {error}")))?;
    Ok(format!(
        "{what}-{}-{}",
        since.as_nanos(),
        std::process::id()
    ))
}
