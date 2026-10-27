//! What the window shares as its screen: a display or a window, as its picker chose, captured by
//! mic-media at the size a screen is sent in. The main display until something is chosen.

use std::sync::{Arc, Mutex, PoisonError};

use mic_media::video::screen::{ScreenCapture, Source};

/// A choice the picker offers.
#[derive(Clone, Debug)]
pub struct Choice {
    pub label: String,
    pub source: Source,
}

/// The chosen screen, which every share captures. Cheap to clone; every clone is the same.
#[derive(Clone, Default)]
pub struct Screens {
    chosen: Arc<Mutex<Option<Source>>>,
}

impl Screens {
    /// What there is to share: each display, then each window. On Wayland, one choice: the
    /// desktop's portal asks which screen, and X11's lists would only hold XWayland's windows,
    /// whose root can't be read.
    pub fn choices() -> Vec<Choice> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return vec![Choice {
                label: "Entire screen (your desktop asks which)".to_owned(),
                source: Source::Display(None),
            }];
        }
        let displays = mic_media::video::screen::displays().unwrap_or_else(|error| {
            tracing::warn!(%error, "listing the displays");
            Vec::new()
        });
        let windows = mic_media::video::screen::windows().unwrap_or_else(|error| {
            tracing::debug!(%error, "listing the windows");
            Vec::new()
        });
        displays
            .into_iter()
            .map(|display| Choice {
                label: format!(
                    "Entire screen: {} ({}×{})",
                    display.name, display.width, display.height
                ),
                source: display.source(),
            })
            .chain(windows.into_iter().map(|window| Choice {
                label: if window.app.is_empty() {
                    format!("Window: {}", window.title)
                } else {
                    format!("Window: {} ({})", window.title, window.app)
                },
                source: window.source(),
            }))
            .collect()
    }

    /// Shares `source` from the next share on.
    pub fn choose(&self, source: Source) {
        *self.chosen.lock().unwrap_or_else(PoisonError::into_inner) = Some(source);
    }

    /// What a share captures (`mic_core::Media::screen`).
    pub fn source(&self) -> mic_core::video::ScreenSource {
        let chosen = self.chosen.clone();
        Arc::new(move |pictures| {
            let source = chosen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or(Source::Display(None));
            let settings = mic_core::video::SCREEN_SETTINGS;
            ScreenCapture {
                source,
                width: settings.width,
                height: settings.height,
                fps: settings.fps,
            }
            .start(pictures)
            .map_err(|error| error.to_string())
        })
    }
}
