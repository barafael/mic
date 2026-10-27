//! The system's microphone and speaker for the app: the speaker plays from when the app starts,
//! and each join opens the microphone afresh, its echo of the speaker cancelled (D14). A device that isn't there or won't open costs only
//! its direction: without a microphone the app listens, without a speaker it speaks.

use std::sync::{Arc, Mutex, PoisonError};

use mic_core::Media;
use mic_media::audio::noise::NoiseSuppression;
use mic_media::audio::{
    EchoCanceller, Microphone, Mixer, Running, Sink as _, Source as _, Speaker, capture,
};

/// The noise suppressor a microphone gets: RNNoise, measured best in the box (PLAN.md, commit
/// 30; design §5.5).
const NOISE_SUPPRESSION: NoiseSuppression = NoiseSuppression::Rnnoise;

pub struct Devices {
    /// The speaker's mixer, if it opened.
    mixer: Option<Mixer>,
    /// What cancels the speaker out of the microphone, if there's a speaker.
    echo: Option<EchoCanceller>,
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
            echo: speaker.is_some().then(|| mixer.echo_canceller()),
            mixer: speaker.is_some().then_some(mixer),
            microphone: Arc::new(Mutex::new(None)),
            _speaker: speaker,
        }
    }

    /// What each join sends and plays (`mic_core::App::with_media`).
    pub fn media(&self) -> impl Fn() -> Media + Send + Sync + 'static {
        let (mixer, microphone, echo) = (
            self.mixer.clone(),
            self.microphone.clone(),
            self.echo.clone(),
        );
        move || {
            let (writer, captured) = capture();
            // The last room's first, so a device that opens once at a time can.
            let mut slot = microphone.lock().unwrap_or_else(PoisonError::into_inner);
            *slot = None;
            let device = match &echo {
                Some(echo) => Microphone::cancelling(echo.clone()),
                None => Microphone::new(),
            };
            let capture = match Box::new(device).start(writer) {
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
                noise: NOISE_SUPPRESSION,
            }
        }
    }
}
