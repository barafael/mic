//! What the window remembers between starts: the name joined with, the microphone and speaker
//! chosen, and the noise suppression. A small JSON file in the user's config directory, read at
//! start and written as each changes; one that won't read is set aside for the defaults, so a
//! settings file can't keep mic from starting.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use mic_media::audio::noise::NoiseSuppression;
use serde::{Deserialize, Serialize};

/// What's kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The name last joined with.
    pub name: String,
    /// The microphone and speaker chosen, by id (mic-media's `Device`); the system's defaults
    /// if none.
    pub microphone: Option<String>,
    pub speaker: Option<String>,
    /// The camera chosen, by id (moq-video's); the first there is if none.
    pub camera: Option<String>,
    /// The noise suppression chosen; RNNoise if none (`devices::NOISE_SUPPRESSION`).
    pub noise: Option<NoiseSuppression>,
}

/// The settings, and where they're kept. Cheap to clone; every clone is the same.
#[derive(Clone)]
pub struct Preferences {
    path: Option<PathBuf>,
    settings: Arc<Mutex<Settings>>,
}

impl Preferences {
    /// The settings kept in `path`, or the defaults if there are none yet; with no path, kept in
    /// memory only (`--ephemeral`, and where there's no config directory).
    pub fn load(path: Option<PathBuf>) -> Self {
        let settings = path
            .as_ref()
            .and_then(|path| match std::fs::read(path) {
                Ok(bytes) => match serde_json::from_slice(&bytes) {
                    Ok(settings) => Some(settings),
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "settings that don't read: the defaults instead");
                        None
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "the settings");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            path,
            settings: Arc::new(Mutex::new(settings)),
        }
    }

    /// Where the settings are kept on this system: mic's directory in the user's config
    /// directory, if there's one.
    pub fn default_path() -> Option<PathBuf> {
        let env = |name| std::env::var_os(name).map(PathBuf::from);
        let directory = if cfg!(windows) {
            env("APPDATA")
        } else if cfg!(target_os = "macos") {
            env("HOME").map(|home| home.join("Library/Application Support"))
        } else {
            env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|home| home.join(".config")))
        }?;
        Some(directory.join("mic").join("settings.json"))
    }

    pub fn get(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Changes the settings as `change` does, and keeps them, if they changed.
    pub fn update(&self, change: impl FnOnce(&mut Settings)) {
        let settings = {
            let mut settings = self.settings.lock().unwrap_or_else(PoisonError::into_inner);
            let before = settings.clone();
            change(&mut settings);
            if *settings == before {
                return;
            }
            settings.clone()
        };
        if let Some(path) = &self.path
            && let Err(error) = write(path, &settings)
        {
            tracing::warn!(%error, path = %path.display(), "keeping the settings");
        }
    }
}

/// `settings` into `path`, whole or not at all: written beside it, then renamed over it.
fn write(path: &std::path::Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let partial = path.with_extension("json.partial");
    std::fs::write(
        &partial,
        serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read_back_as_kept_and_a_broken_file_is_the_defaults() {
        let directory = std::env::temp_dir().join(format!("mic-settings-{}", std::process::id()));
        let path = directory.join("mic/settings.json");
        let preferences = Preferences::load(Some(path.clone()));
        assert_eq!(preferences.get(), Settings::default());
        preferences.update(|settings| {
            settings.name = "Alice".to_owned();
            settings.microphone = Some("alsa:hw:1".to_owned());
            settings.noise = Some(NoiseSuppression::Off);
        });
        let again = Preferences::load(Some(path.clone()));
        assert_eq!(again.get().name, "Alice");
        assert_eq!(again.get().noise, Some(NoiseSuppression::Off));
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Preferences::load(Some(path)).get(), Settings::default());
        // An older file, without what's newer: the defaults for that.
        let old = directory.join("old.json");
        std::fs::write(&old, br#"{ "name": "Bob" }"#).unwrap();
        assert_eq!(Preferences::load(Some(old)).get().name, "Bob");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
