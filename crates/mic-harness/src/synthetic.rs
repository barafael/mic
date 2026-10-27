//! The box's microphone, speaker and camera: synthetic ones, so a check knows what a client sends
//! and can ask what it heard and saw. Each join sends a tone of the client's own, and every other
//! voice plays into an analyzer, which the harness answers [`Call::Heard`](crate::Call::Heard)
//! from; the camera shows a pattern naming the client (mic-media's `PatternCamera`), which a
//! check reads back from what the others decoded ([`Call::Seen`](crate::Call::Seen)).

use std::sync::{Arc, Mutex, PoisonError};

use mic_core::Media;
use mic_media::audio::{Analyzer, AnalyzingSink, Mixer, Running, Sink, Source, ToneSource};
use mic_media::video::PatternCamera;

/// A tone for a microphone, if it has one, and an analyzer for a speaker. The speaker plays for
/// as long as this lives.
pub struct Synthetic {
    pub analyzer: Analyzer,
    mixer: Mixer,
    tone: Option<f64>,
    /// The tone of the room it's in: a join starts a new one, which stops the last.
    microphone: Arc<Mutex<Option<Running>>>,
    _speaker: Running,
    /// The camera's pictures, and the camera, which draws for as long as this lives.
    camera: Option<(mic_core::video::Pictures, Running)>,
}

impl Synthetic {
    /// A speaker that analyzes, a microphone that sends `tone` Hz, if any, and a camera that
    /// shows `camera`'s pattern, if any, at the size and rate mic sends a camera in.
    pub fn new(tone: Option<f64>, camera: Option<u8>) -> Result<Self, mic_media::audio::Error> {
        let mixer = Mixer::new();
        let (sink, analyzer) = AnalyzingSink::new();
        let speaker = Box::new(sink).start(mixer.clone())?;
        let camera = camera
            .map(|sender| {
                let (pictures, shown) = tokio::sync::watch::channel(None);
                let settings = mic_core::video::CAMERA_SETTINGS;
                PatternCamera {
                    sender,
                    width: settings.width,
                    height: settings.height,
                    fps: settings.fps,
                }
                .start(pictures)
                .map(|running| (shown, running))
            })
            .transpose()?;
        Ok(Self {
            analyzer,
            mixer,
            tone,
            microphone: Arc::new(Mutex::new(None)),
            _speaker: speaker,
            camera,
        })
    }

    /// What each join sends and plays (`mic_core::App::with_media`).
    pub fn media(&self) -> impl Fn() -> Media + Send + Sync + 'static {
        let (mixer, tone, microphone) = (self.mixer.clone(), self.tone, self.microphone.clone());
        let camera = self.camera.as_ref().map(|(pictures, _)| pictures.clone());
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
            }
        }
    }
}
