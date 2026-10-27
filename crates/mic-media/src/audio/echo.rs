//! Echo cancellation (D14): keeps what the speaker plays out of what the microphone sends, with
//! sonora's AEC3, a pure Rust port of WebRTC's (no libwebrtc, no C++).
//!
//! The mixer taps what it plays into a reference ring ([`Mixer::echo_canceller`]); the
//! microphone's callback hands the canceller its samples, which first takes in everything the
//! mixer played since, then cancels it from the microphone, 10 ms at a time. Both halves run in
//! that order on the microphone's thread, as the echo model needs, so nothing is split across
//! the two devices' callbacks. That adds up to 10 ms to the capture path.
//!
//! The reference is never queued deliberately: what's read is all the mixer produced since the
//! last callback, so it leads its echo by however much the output device buffers. A queue
//! standing in front of it would be a delay the model would have to find on top of that.

use std::sync::{Arc, Mutex, PoisonError};

use ringbuf::HeapCons;
use ringbuf::traits::{Consumer as _, Observer as _};
use sonora::config::EchoCanceller as Aec3;
use sonora::{AudioProcessing, StreamConfig};

use super::SAMPLE_RATE;

/// Samples a frame of the canceller's: 10 ms.
const FRAME: usize = SAMPLE_RATE as usize / 100;

/// An echo canceller for one microphone, against one mixer's playback. Cheap to clone; every
/// clone is the same canceller.
#[derive(Clone)]
pub struct EchoCanceller {
    state: Arc<Mutex<State>>,
}

struct State {
    processor: AudioProcessing,
    /// What the mixer played, as it played it.
    reference: HeapCons<f32>,
    /// Microphone samples waiting for a whole frame.
    pending: Vec<f32>,
    /// Cancelled samples not yet handed back.
    processed: Vec<f32>,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
}

impl EchoCanceller {
    /// A canceller reading what was played from `reference`.
    pub(crate) fn new(reference: HeapCons<f32>) -> Self {
        let config = StreamConfig::new(SAMPLE_RATE, 1);
        let processor = AudioProcessing::builder()
            .capture_config(config)
            .render_config(config)
            .config(sonora::Config {
                echo_canceller: Some(Aec3::default()),
                ..Default::default()
            })
            .build();
        Self {
            state: Arc::new(Mutex::new(State {
                processor,
                reference,
                // Room for a large callback's worth, so the microphone's thread doesn't grow them.
                pending: Vec::with_capacity(8 * FRAME),
                processed: Vec::with_capacity(8 * FRAME),
                frame_in: vec![0.0; FRAME],
                frame_out: vec![0.0; FRAME],
            })),
        }
    }

    /// Replaces `samples`, from the microphone, with the same span less the echo of what was
    /// played: the first 10 ms come out silent while the first frame fills. If the canceller is
    /// busy (another microphone got hold of it), they pass through.
    pub fn process(&self, samples: &mut [f32]) {
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        state.process(samples);
    }

    /// Takes in what was played and drops it, while the microphone doesn't run: so when one
    /// opens, the model starts from what plays then.
    pub fn idle(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let available = state.reference.occupied_len();
        state.reference.skip(available);
        state.pending.clear();
        state.processed.clear();
    }
}

impl State {
    fn process(&mut self, samples: &mut [f32]) {
        let State {
            processor,
            reference,
            pending,
            processed,
            frame_in,
            frame_out,
        } = self;
        // What was played first: the model must hear it before it looks for it.
        while reference.occupied_len() >= FRAME {
            reference.pop_slice(frame_in);
            if let Err(error) = processor.process_render_f32(&[frame_in], &mut [frame_out]) {
                tracing::debug!(?error, "the echo reference");
            }
        }
        pending.extend_from_slice(samples);
        let mut at = 0;
        while pending.len() - at >= FRAME {
            frame_in.copy_from_slice(&pending[at..at + FRAME]);
            match processor.process_capture_f32(&[frame_in], &mut [frame_out]) {
                Ok(()) => processed.extend_from_slice(frame_out),
                // A format sonora rejects, it rejects every time: the microphone as it is.
                Err(_) => processed.extend_from_slice(frame_in),
            }
            at += FRAME;
        }
        pending.drain(..at);
        let ready = processed.len().min(samples.len());
        samples[..ready].copy_from_slice(&processed[..ready]);
        samples[ready..].fill(0.0);
        processed.drain(..ready);
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;
    use crate::audio::{Mixer, level};

    /// `count` samples of a tone at `frequency`, `amplitude` loud, from sample `from`.
    fn tone(frequency: f64, amplitude: f64, from: usize, count: usize) -> Vec<f32> {
        (from..from + count)
            .map(|i| {
                ((TAU * frequency * i as f64 / f64::from(SAMPLE_RATE)).sin() * amplitude) as f32
            })
            .collect()
    }

    /// The near end's voice: 660 Hz at 0.1, about speech's -20 dBFS. (AEC3 takes a near-end tone
    /// at -6 dBFS, talking over the far end, for echo, and suppresses it: a tone's no voice,
    /// but it says to measure double talk with speech in the box, PLAN.md commit 30.)
    const VOICE: f64 = 0.1;

    /// A room: the far end's 440 Hz at 0.5 plays from the speaker and comes back into the
    /// microphone 30 ms later at half its level, over the near end's voice, if `talking`.
    /// Returns the last second the microphone sent, after `seconds` of it.
    fn echo(seconds: usize, cancel: bool, talking: bool) -> Vec<f32> {
        let mixer = Mixer::new();
        let canceller = mixer.echo_canceller();
        let mut far = mixer.stream();
        let delay = SAMPLE_RATE as usize * 30 / 1000;
        let mut played: Vec<f32> = vec![0.0; delay];
        let mut sent = Vec::new();
        let chunk = FRAME * 2;
        for at in (0..seconds * SAMPLE_RATE as usize).step_by(chunk) {
            far.write(&tone(440.0, 0.5, at, chunk));
            let mut out = vec![0.0; chunk];
            mixer.fill(&mut out);
            played.extend_from_slice(&out);
            let near = tone(660.0, if talking { VOICE } else { 0.0 }, at, chunk);
            let mut microphone: Vec<f32> = near
                .iter()
                .zip(&played[at..at + chunk])
                .map(|(voice, echo)| voice + echo * 0.5)
                .collect();
            if cancel {
                canceller.process(&mut microphone);
            }
            sent.extend(microphone);
        }
        sent.split_off(sent.len() - SAMPLE_RATE as usize)
    }

    #[test]
    fn the_echo_of_what_played_is_cancelled() {
        let raw = echo(6, false, false);
        assert!(
            level(&raw, 440.0) > 0.2,
            "the echo is there: {}",
            level(&raw, 440.0)
        );
        let cancelled = echo(6, true, false);
        let echoed = level(&cancelled, 440.0);
        assert!(echoed < level(&raw, 440.0) / 100.0, "the echo at {echoed}");
    }

    #[test]
    fn a_voice_over_the_echo_is_kept() {
        let cancelled = echo(6, true, true);
        let (echoed, voice) = (level(&cancelled, 440.0), level(&cancelled, 660.0));
        assert!(echoed < 0.01, "the echo at {echoed}");
        assert!(voice > VOICE * 0.7, "the voice at {voice}");
    }
}
