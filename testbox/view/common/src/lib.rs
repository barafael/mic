//! What the box's display tools share: failures typed, with what was being done; running a
//! command; the viewer's input on a display; and the check that a display's socket is the box's.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::Path;
use std::process::Command;

use derive_more::{Display, Error};

#[derive(Debug, Display, Error)]
pub enum Error {
    /// I/O while doing `what`.
    #[display("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    /// JSON that didn't parse or serialize: a message of the web viewer's page.
    #[display("{what}: {source}")]
    Json {
        what: String,
        source: serde_json::Error,
    },
    /// Another library's failure; their error types differ, so their message is what is kept.
    #[display("{what}: {source}")]
    Format {
        what: String,
        #[error(not(source))]
        source: String,
    },
    /// A command that failed, with its status.
    #[display("{command} failed ({status})")]
    Command {
        command: String,
        #[error(not(source))]
        status: String,
    },
    /// The rest: a refused request, a wait that timed out, a state that isn't as expected.
    #[display("{_0}")]
    Failed(#[error(not(source))] String),
}

impl From<&str> for Error {
    fn from(failure: &str) -> Self {
        Self::Failed(failure.to_owned())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Says what was being done when an I/O or JSON error came up.
pub trait Context<T> {
    fn context(self, what: impl Into<String>) -> Result<T>;
}

impl<T> Context<T> for std::io::Result<T> {
    fn context(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Io {
            what: what.into(),
            source,
        })
    }
}

impl<T> Context<T> for serde_json::Result<T> {
    fn context(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Json {
            what: what.into(),
            source,
        })
    }
}

/// Says what was being done when another library failed, with the error's causes after it.
pub trait Describe<T> {
    fn describe(self, what: impl Into<String>) -> Result<T>;
}

impl<T, E: std::error::Error> Describe<T> for std::result::Result<T, E> {
    fn describe(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|error| Error::Format {
            what: what.into(),
            source: std::iter::successors(error.source(), |cause| cause.source())
                .fold(error.to_string(), |message, cause| {
                    format!("{message}: {cause}")
                }),
        })
    }
}

/// A tool's exit: its failure on stderr, under its name.
pub fn exit(tool: &str, outcome: Result<()>) -> std::process::ExitCode {
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{tool}: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Runs `command` with the terminal attached; fails on a non-zero exit.
pub fn run(mut command: Command) -> Result<()> {
    let described = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let status = command.status().context(described.clone())?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Command {
            command: described,
            status: status.to_string(),
        })
    }
}

/// The viewer's input on a display (`with-display input`, the xtask's viewers).
#[derive(Clone, Copy, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Input {
    On,
    Off,
}

/// Fails unless `socket` is a socket owned by its directory's owner, where the box's clients put
/// theirs: the directory is theirs to write too, and a link planted there would lead a viewer of
/// it to any socket of the host's.
pub fn box_socket(socket: &Path) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let directory = socket
        .parent()
        .ok_or_else(|| Error::Failed(format!("{} has no directory", socket.display())))?;
    let owner = std::fs::metadata(directory)
        .context(directory.display().to_string())?
        .uid();
    let found = std::fs::symlink_metadata(socket).context(socket.display().to_string())?;
    if found.file_type().is_socket() && found.uid() == owner {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{} is not a socket of the box's (a {:?} of uid {})",
            socket.display(),
            found.file_type(),
            found.uid()
        )))
    }
}

pub fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context(path.display().to_string()),
    }
}
