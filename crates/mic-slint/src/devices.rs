//! The system's microphone and speaker for the app: the speaker plays from when the app starts,
//! and each join opens the microphone afresh. A device that isn't there or won't open costs only
//! its direction: without a microphone the app listens, without a speaker it speaks.

use std::sync::{Arc, Mutex, PoisonError};

use mic_core::Media;
use mic_media::audio::{Microphone, Mixer, Running, Sink as _, Source as _, Speaker, capture};

pub struct Devices {
    /// The speaker's mixer, if it opened.
    mixer: Option<Mixer>,
    /// The microphone of the room it's in: a join opens a new one, which closes the last.
    microphone: Arc<Mutex<Option<Running>>>,
    _speaker: Option<Running>,
}

impl Devices {
    pub fn new() -> Self {
        let mixer = Mixer::new();
        let speaker = match Box::new(Speaker).start(mixer.clone()) {
            Ok(running) => Some(running),
            Err(error) => {
                tracing::warn!(%error, "no speaker: nothing is played");
                None
            }
        };
        Self {
            mixer: speaker.is_some().then_some(mixer),
            microphone: Arc::new(Mutex::new(None)),
            _speaker: speaker,
        }
    }

    /// What each join sends and plays (`mic_core::App::with_media`).
    pub fn media(&self) -> impl Fn() -> Media + Send + Sync + 'static {
        let (mixer, microphone) = (self.mixer.clone(), self.microphone.clone());
        move || {
            let (writer, captured) = capture();
            // The last room's first, so a device that opens once at a time can.
            let mut slot = microphone.lock().unwrap_or_else(PoisonError::into_inner);
            *slot = None;
            let capture = match Box::new(Microphone).start(writer) {
                Ok(running) => {
                    *slot = Some(running);
                    Some(captured)
                }
                Err(error) => {
                    tracing::warn!(%error, "no microphone: this join sends no voice");
                    None
                }
            };
            Media {
                capture,
                mixer: mixer.clone(),
            }
        }
    }
}
