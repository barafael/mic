//! How the box was brought up (`up`'s flags), kept beside the pods' files, so whatever plays the
//! pods again later (`dev`, `run --fresh`) plays the same box, not the defaults.

use std::path::{Path, PathBuf};

use crate::error::{self, Error, Result};

/// Where they're kept, in the rendered pods' directory.
const FILE: &str = "target/testbox-pods/box.settings";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Whether client pods get the host's GPU (`up --no-gpu` says no).
    pub gpu: bool,
    /// How many the crowd pod runs (`up --crowd n`): with the six windows, a room of 40.
    pub crowd: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gpu: true,
            crowd: 34,
        }
    }
}

impl Settings {
    fn file(repo: &Path) -> PathBuf {
        repo.join(FILE)
    }

    /// The settings `up` last kept, or the defaults if it never ran here.
    pub fn saved(repo: &Path) -> Result<Self> {
        let file = Self::file(repo);
        if !file.exists() {
            return Ok(Self::default());
        }
        Self::parse(&error::read_to_string(&file)?)
            .map_err(|why| Error::Failed(format!("{}: {why}", file.display())))
    }

    pub fn save(self, repo: &Path) -> Result<()> {
        let file = Self::file(repo);
        if let Some(directory) = file.parent() {
            error::create_dir_all(directory)?;
        }
        error::write(&file, self.to_text())
    }

    /// `key = value` lines.
    fn to_text(self) -> String {
        format!("gpu = {}\ncrowd = {}\n", self.gpu, self.crowd)
    }

    fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut settings = Self::default();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("{line:?} isn't `key = value`"))?;
            match (key.trim(), value.trim()) {
                ("gpu", value) => {
                    settings.gpu = value.parse().map_err(|_| format!("gpu = {value:?}"))?;
                }
                ("crowd", value) => {
                    settings.crowd = value.parse().map_err(|_| format!("crowd = {value:?}"))?;
                }
                (key, _) => return Err(format!("no setting {key:?}")),
            }
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn settings_read_back_as_kept() {
        let settings = Settings {
            gpu: false,
            crowd: 7,
        };
        assert_eq!(Settings::parse(&settings.to_text()), Ok(settings));
    }

    #[test]
    fn an_unknown_setting_is_refused() {
        assert!(Settings::parse("clients = 3\n").is_err());
    }
}
