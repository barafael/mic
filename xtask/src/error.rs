//! What can go wrong in the xtask: failures typed, with what was being done.

use derive_more::{Display, Error};

#[derive(Debug, Display, Error)]
pub enum Error {
    /// I/O while doing `what`.
    #[display("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    /// A command that failed, with what it printed.
    #[display("{command} failed{output}")]
    Command {
        command: String,
        #[error(not(source))]
        output: String,
    },
    /// The rest: a check that didn't hold, a wait that timed out, a state that isn't as expected.
    #[display("{_0}")]
    Failed(#[error(not(source))] String),
}

impl From<&str> for Error {
    fn from(failure: &str) -> Self {
        Self::Failed(failure.to_owned())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Says what was being done when an I/O error came up.
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

pub fn read(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).context(path.display().to_string())
}

pub fn read_to_string(path: &std::path::Path) -> Result<String> {
    std::fs::read_to_string(path).context(path.display().to_string())
}

pub fn write(path: &std::path::Path, contents: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, contents).context(path.display().to_string())
}

pub fn create_dir_all(path: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(path).context(path.display().to_string())
}
