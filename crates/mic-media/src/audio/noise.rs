//! Noise suppression (D14): steady background noise (a fan, hum, the street) taken out of the
//! microphone before it's sent. Two candidates, chosen by measuring them (PLAN.md, commit 30):
//!
//! - [`NoiseSuppression::Rnnoise`]: `nnnoiseless`, a Rust port of Xiph's RNNoise, a small
//!   recurrent network; what Jitsi Meet uses by default.
//! - [`NoiseSuppression::Sonora`]: sonora's, WebRTC's Wiener filter with its own voice detection,
//!   at its high level.
//!
//! Both take 10 ms frames at 48 kHz, so a 20 ms frame is two of them, and neither adds latency
//! here. Both would take a steady tone for noise, and remove it: they're for a microphone, not
//! for the box's synthetic voices. What they're measured with is [`speech_like`] over [`noise`],
//! by [`voice_to_noise`].

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};
use sonora::config::{NoiseSuppression as SonoraNs, NoiseSuppressionLevel};
use sonora::{AudioProcessing, StreamConfig};

use super::SAMPLE_RATE;

/// Samples a suppressor's frame: 10 ms.
const FRAME: usize = SAMPLE_RATE as usize / 100;

/// Which suppressor, if any.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseSuppression {
    #[default]
    Off,
    Rnnoise,
    Sonora,
}

/// A suppressor's state: it learns the noise as it goes, so one runs per stream.
pub struct Suppressor {
    kind: Kind,
}

enum Kind {
    Off,
    Rnnoise {
        state: Box<nnnoiseless::DenoiseState<'static>>,
        scaled: Vec<f32>,
        out: Vec<f32>,
        /// RNNoise's first frame comes out of a cold state: it's dropped for silence.
        started: bool,
    },
    Sonora {
        processor: Box<AudioProcessing>,
        out: Vec<f32>,
    },
}

impl Suppressor {
    pub fn new(mode: NoiseSuppression) -> Self {
        let kind = match mode {
            NoiseSuppression::Off => Kind::Off,
            NoiseSuppression::Rnnoise => Kind::Rnnoise {
                state: nnnoiseless::DenoiseState::new(),
                scaled: vec![0.0; FRAME],
                out: vec![0.0; FRAME],
                started: false,
            },
            NoiseSuppression::Sonora => {
                let config = StreamConfig::new(SAMPLE_RATE, 1);
                Kind::Sonora {
                    processor: Box::new(
                        AudioProcessing::builder()
                            .capture_config(config)
                            .render_config(config)
                            .config(sonora::Config {
                                noise_suppression: Some(SonoraNs {
                                    level: NoiseSuppressionLevel::High,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            })
                            .build(),
                    ),
                    out: vec![0.0; FRAME],
                }
            }
        };
        Self { kind }
    }

    /// Takes the noise out of `samples`, in place: a whole number of 10 ms frames (a 20 ms frame
    /// is two). A remainder is left as it is.
    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<FRAME>().0 {
            match &mut self.kind {
                Kind::Off => {}
                Kind::Rnnoise {
                    state,
                    scaled,
                    out,
                    started,
                } => {
                    // In the range of 16-bit samples, as RNNoise was trained.
                    for (scaled, &sample) in scaled.iter_mut().zip(frame.iter()) {
                        *scaled = sample * 32768.0;
                    }
                    state.process_frame(out, scaled);
                    if *started {
                        for (sample, &out) in frame.iter_mut().zip(out.iter()) {
                            *sample = out / 32768.0;
                        }
                    } else {
                        frame.fill(0.0);
                        *started = true;
                    }
                }
                Kind::Sonora { processor, out } => {
                    match processor.process_capture_f32(&[frame], &mut [out]) {
                        Ok(()) => frame.copy_from_slice(out),
                        Err(error) => tracing::debug!(?error, "noise suppression"),
                    }
                }
            }
        }
    }
}

/// The voice the suppressors are measured with: a vowel-like sound, the harmonics of
/// [`VOICE_PITCH`] to 3 kHz, falling 1/k, in syllables of 250 ms at four a second, with a quarter
/// second's pause each second, as speech pauses; about -23 dBFS. A tone would be taken for
/// noise; this isn't.
pub fn speech_like(from: usize, count: usize) -> Vec<f32> {
    (from..from + count)
        .map(|i| {
            let t = i as f64 / f64::from(SAMPLE_RATE);
            let second = t.fract();
            let envelope = if second < 0.75 {
                (std::f64::consts::PI * second / 0.25).sin().powi(2)
            } else {
                0.0
            };
            let voiced: f64 = (1..=HARMONICS)
                .map(|k| (TAU * VOICE_PITCH * k as f64 * t).sin() / k as f64)
                .sum();
            (voiced * envelope * 0.06) as f32
        })
        .collect()
}

/// The voice's fundamental, in Hz.
pub const VOICE_PITCH: f64 = 150.0;
/// The voice's harmonics: to 3 kHz.
pub const HARMONICS: usize = 20;

/// White noise, `rms` loud, from a fixed seed `seed`, so a run is repeatable.
pub fn noise(seed: u64, count: usize, rms: f32) -> Vec<f32> {
    let mut state = seed | 1;
    // Uniform in -1..1 has an RMS of 1/sqrt(3).
    let scale = rms * 3.0_f32.sqrt();
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32 * scale
        })
        .collect()
}

/// The frequencies [`voice_to_noise`] takes levels at: the voice's harmonics, then the points
/// halfway between them, where only noise is.
pub fn measured_frequencies() -> Vec<f64> {
    let harmonics = (1..=HARMONICS).map(|k| VOICE_PITCH * k as f64);
    let between = (1..=HARMONICS).map(|k| VOICE_PITCH * (k as f64 + 0.5));
    harmonics.chain(between).collect()
}

/// The voice-to-noise ratio, in dB, from levels at [`measured_frequencies`]: the power at the
/// voice's harmonics against the power halfway between them. Needs no timing, so it holds for
/// what's heard at the far end of a call as well as here.
pub fn voice_to_noise(levels: &[f64]) -> f64 {
    let (harmonics, between) = levels.split_at(levels.len() / 2);
    10.0 * (power(harmonics) / power(between).max(1e-20)).log10()
}

/// The voice's own power, in dB, from levels at [`measured_frequencies`]: what a suppressor took
/// of the voice shows as a fall in it.
pub fn voice_power(levels: &[f64]) -> f64 {
    10.0 * power(&levels[..levels.len() / 2]).max(1e-20).log10()
}

fn power(levels: &[f64]) -> f64 {
    levels.iter().map(|level| level * level).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::level;

    /// The last second of `seconds` of the voice over noise, through `mode`: its voice-to-noise
    /// ratio, and the voice's power.
    fn through(mode: NoiseSuppression, seconds: usize) -> (f64, f64) {
        let count = seconds * SAMPLE_RATE as usize;
        let noise = noise(7, count, 0.02);
        let mut signal: Vec<f32> = speech_like(0, count)
            .iter()
            .zip(&noise)
            .map(|(voice, noise)| voice + noise)
            .collect();
        let mut suppressor = Suppressor::new(mode);
        for frame in signal.chunks_mut(2 * FRAME) {
            suppressor.process(frame);
        }
        let last = &signal[count - SAMPLE_RATE as usize..];
        let levels: Vec<f64> = measured_frequencies()
            .into_iter()
            .map(|frequency| level(last, frequency))
            .collect();
        (voice_to_noise(&levels), voice_power(&levels))
    }

    #[test]
    fn each_suppressor_raises_the_voice_over_the_noise_and_keeps_it() {
        let (off, voice) = through(NoiseSuppression::Off, 6);
        for mode in [NoiseSuppression::Rnnoise, NoiseSuppression::Sonora] {
            let (on, kept) = through(mode, 6);
            eprintln!(
                "{mode:?}: the voice {:.1} dB further over the noise than off, and {:.1} dB itself",
                on - off,
                kept - voice
            );
            assert!(
                on > off + 3.0,
                "{mode:?}: {on:.1} dB against {off:.1} dB off"
            );
            assert!(
                kept > voice - 6.0,
                "{mode:?}: the voice {:.1} dB",
                kept - voice
            );
        }
    }

    #[test]
    fn noise_is_as_loud_as_asked() {
        let noise = noise(3, SAMPLE_RATE as usize, 0.1);
        let rms =
            (noise.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / noise.len() as f64).sqrt();
        assert!((rms - 0.1).abs() < 0.005, "{rms}");
    }
}
