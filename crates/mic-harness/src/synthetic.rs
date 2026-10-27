//! The box's microphone and speaker: synthetic ones, so a check knows what a client sends and
//! can ask what it heard. Each join sends a tone of the client's own, and every other voice plays
//! into an analyzer, which the harness answers [`Call::Heard`](crate::Call::Heard) from.

use std::sync::{Arc, Mutex, PoisonError};

use mic_core::Media;
use mic_media::audio::{Analyzer, AnalyzingSink, Mixer, Running, Sink, Source, ToneSource};

/// A tone for a microphone, if it has one, and an analyzer for a speaker. The speaker plays for
/// as long as this lives.
pub struct Synthetic {
    pub analyzer: Analyzer,
    mixer: Mixer,
    tone: Option<f64>,
    /// The tone of the room it's in: a join starts a new one, which stops the last.
    microphone: Arc<Mutex<Option<Running>>>,
    _speaker: Running,
}

impl Synthetic {
    /// A speaker that analyzes, and a microphone that sends `tone` Hz, if any.
    pub fn new(tone: Option<f64>) -> Result<Self, mic_media::audio::Error> {
        let mixer = Mixer::new();
        let (sink, analyzer) = AnalyzingSink::new();
        let speaker = Box::new(sink).start(mixer.clone())?;
        Ok(Self {
            analyzer,
            mixer,
            tone,
            microphone: Arc::new(Mutex::new(None)),
            _speaker: speaker,
        })
    }

    /// What each join sends and plays (`mic_core::App::with_media`).
    pub fn media(&self) -> impl Fn() -> Media + Send + Sync + 'static {
        let (mixer, tone, microphone) = (self.mixer.clone(), self.tone, self.microphone.clone());
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
            }
        }
    }
}
