//! The box's microphone, speaker and camera: synthetic ones, so a check knows what a client sends
//! and can ask what it heard and saw. Each join sends a tone of the client's own, and every other
//! voice plays into an analyzer, which the harness answers [`Call::Heard`](crate::Call::Heard)
//! from; the camera shows a pattern naming the client (mic-media's `PatternCamera`), which a
//! check reads back from what the others decoded ([`Call::Seen`](crate::Call::Seen)).

use std::sync::{Arc, Mutex, PoisonError};

use mic_core::Media;
use mic_media::audio::{Analyzer, AnalyzingSink, Mixer, Running, Sink, Source, ToneSource};
use mic_media::video::{Pattern, PatternCamera};

/// A tone for a microphone, if it has one, and an analyzer for a speaker. The speaker plays for
/// as long as this lives.
pub struct Synthetic {
    pub analyzer: Analyzer,
    mixer: Mixer,
    tone: Option<f64>,
    /// The tone of the room it's in: a join starts a new one, which stops the last.
    microphone: Arc<Mutex<Option<Running>>>,
    _speaker: Running,
    /// What it sends as a camera, and the camera that draws it live, for as long as this lives.
    camera: Option<(mic_core::video::Camera, Option<Running>)>,
    /// The pattern its screen shows, if it shares one.
    screen: Option<u8>,
}

/// The pattern a synthetic screen shows, for a camera's `sender`: apart from every camera's.
pub fn screen_pattern(sender: u8) -> u8 {
    sender.wrapping_add(100)
}

/// How a synthetic camera is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sending {
    /// Drawn and encoded as it goes, as a real camera is.
    Live,
    /// Encoded once, two seconds of it, and sent in a loop: a crowd's, without an encoder each.
    Recorded,
}

/// How many seconds a recorded camera loops.
const RECORDED_SECONDS: u32 = 2;

impl Synthetic {
    /// A speaker that analyzes, a microphone that sends `tone` Hz, if any, and a camera that
    /// shows `camera`'s pattern, if any, at the size and rate mic sends a camera in.
    pub fn new(tone: Option<f64>, camera: Option<u8>) -> Result<Self, String> {
        Self::sending(tone, camera, Sending::Live)
    }

    /// As [`Self::new`], its camera sent as `sending` says.
    pub fn sending(
        tone: Option<f64>,
        camera: Option<u8>,
        sending: Sending,
    ) -> Result<Self, String> {
        // One with a camera shares a screen with a pattern of its own.
        let screen = camera.map(screen_pattern);
        let mixer = Mixer::new();
        let (sink, analyzer) = AnalyzingSink::new();
        let speaker = Box::new(sink)
            .start(mixer.clone())
            .map_err(|error| error.to_string())?;
        let settings = mic_core::video::CAMERA_SETTINGS;
        let camera = match (camera, sending) {
            (None, _) => None,
            (Some(sender), Sending::Live) => {
                let (pictures, shown) = tokio::sync::watch::channel(None);
                let running = PatternCamera {
                    sender,
                    width: settings.width,
                    height: settings.height,
                    fps: settings.fps,
                }
                .start(pictures)
                .map_err(|error| error.to_string())?;
                Some((mic_core::video::Camera::Live(shown), Some(running)))
            }
            (Some(sender), Sending::Recorded) => {
                let pictures = (0..settings.fps * RECORDED_SECONDS).map(|frame| {
                    Pattern {
                        sender,
                        frame: frame as u16,
                    }
                    .draw(settings.width, settings.height)
                });
                let recorded = mic_core::video::Recorded::encode(pictures)
                    .map_err(|error| format!("the recorded camera: {error}"))?;
                Some((mic_core::video::Camera::Recorded(recorded), None))
            }
        };
        Ok(Self {
            analyzer,
            mixer,
            tone,
            microphone: Arc::new(Mutex::new(None)),
            _speaker: speaker,
            screen,
            camera,
        })
    }

    /// What each join sends and plays (`mic_core::App::with_media`).
    pub fn media(&self) -> impl Fn() -> Media + Send + Sync + 'static {
        let (mixer, tone, microphone) = (self.mixer.clone(), self.tone, self.microphone.clone());
        let camera = self.camera.as_ref().map(|(camera, _)| camera.clone());
        let screen = self.screen.map(|sender| -> mic_core::video::ScreenSource {
            std::sync::Arc::new(move |pictures| {
                let settings = mic_core::video::SCREEN_SETTINGS;
                PatternCamera {
                    sender,
                    width: settings.width,
                    height: settings.height,
                    fps: settings.fps,
                }
                .start(pictures)
                .map_err(|error| error.to_string())
            })
        });
        move || {
            let capture = tone.and_then(|tone| {
                let (writer, capture) = mic_media::audio::capture();
                match Box::new(ToneSource::new(tone)).start(writer) {
                    Ok(running) => {
                        *microphone.lock().unwrap_or_else(PoisonError::into_inner) = Some(running);
                        Some(capture)
                    }
                    Err(error) => {
                        tracing::warn!(%error, "no tone: this join sends no voice");
                        None
                    }
                }
            });
            // No noise suppression: it would take a steady tone for noise.
            Media {
                capture,
                mixer: Some(mixer.clone()),
                noise: Default::default(),
                camera: camera.clone(),
                screen: screen.clone(),
                camera_on: None,
            }
        }
    }
}
