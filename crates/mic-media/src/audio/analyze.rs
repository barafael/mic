//! A synthetic speaker that listens: it plays what the mixer gives into a window of the last
//! second, and says how loud each frequency is in it, so a check can tell whose tone it hears.

use std::collections::VecDeque;
use std::f64::consts::TAU;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::{Error, Mixer, Running, SAMPLE_RATE, Sink, paced};

/// How often it plays: 10 ms, as a device's callback comes.
const PERIOD: Duration = Duration::from_millis(10);
/// What it keeps: the last second, which resolves frequencies 1 Hz apart.
const WINDOW: usize = SAMPLE_RATE as usize;

/// The sink; its [`Analyzer`] reads what it heard.
pub struct AnalyzingSink {
    heard: Arc<Mutex<Heard>>,
}

#[derive(Default)]
struct Heard {
    window: VecDeque<f32>,
    /// Every sample played, since it started.
    total: u64,
}

impl AnalyzingSink {
    pub fn new() -> (Self, Analyzer) {
        let heard = Arc::new(Mutex::new(Heard::default()));
        (
            Self {
                heard: heard.clone(),
            },
            Analyzer { heard },
        )
    }
}

impl Sink for AnalyzingSink {
    fn start(self: Box<Self>, mixer: Mixer) -> Result<Running, Error> {
        let mut played = vec![0.0; super::samples(PERIOD)];
        paced("mic-analyzer", PERIOD, move || {
            mixer.fill(&mut played);
            let mut heard = self.heard.lock().unwrap_or_else(PoisonError::into_inner);
            heard.window.extend(&played);
            let over = heard.window.len().saturating_sub(WINDOW);
            heard.window.drain(..over);
            heard.total += played.len() as u64;
        })
    }
}

/// What an [`AnalyzingSink`] heard, in the last second.
#[derive(Clone)]
pub struct Analyzer {
    heard: Arc<Mutex<Heard>>,
}

impl Analyzer {
    /// The amplitude of `frequency` (Hz): about a sine's own amplitude for a tone at an
    /// integer frequency, about 0 for one it doesn't hold. Goertzel, over the window.
    pub fn level(&self, frequency: f64) -> f64 {
        let heard = self.heard.lock().unwrap_or_else(PoisonError::into_inner);
        goertzel(heard.window.iter().copied(), heard.window.len(), frequency)
    }

    /// The window's loudness: its root mean square.
    pub fn rms(&self) -> f64 {
        let heard = self.heard.lock().unwrap_or_else(PoisonError::into_inner);
        let squares: f64 = heard.window.iter().map(|&s| f64::from(s).powi(2)).sum();
        (squares / heard.window.len().max(1) as f64).sqrt()
    }

    /// How long it has played, in samples.
    pub fn played(&self) -> u64 {
        self.heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .total
    }
}

/// The amplitude of `frequency` (Hz) in `samples` at 48 kHz: about a sine's own amplitude for a
/// tone at that frequency, about 0 without one. For what's recorded elsewhere, as the box records
/// a speaker.
pub fn level(samples: &[f32], frequency: f64) -> f64 {
    goertzel(samples.iter().copied(), samples.len(), frequency)
}

/// The amplitude of `frequency` in `count` samples, by Goertzel's algorithm.
fn goertzel(samples: impl Iterator<Item = f32>, count: usize, frequency: f64) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let coefficient = 2.0 * (TAU * frequency / f64::from(SAMPLE_RATE)).cos();
    let (mut previous, mut before) = (0.0_f64, 0.0_f64);
    for sample in samples {
        let current = f64::from(sample) + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    let power = previous * previous + before * before - coefficient * previous * before;
    2.0 * power.max(0.0).sqrt() / count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frequency: f64, amplitude: f64, count: usize) -> impl Iterator<Item = f32> {
        (0..count).map(move |i| {
            (amplitude * (TAU * frequency * i as f64 / f64::from(SAMPLE_RATE)).sin()) as f32
        })
    }

    #[test]
    fn goertzel_measures_a_tone_and_ignores_another() {
        let mixed: Vec<f32> = sine(440.0, 0.5, WINDOW)
            .zip(sine(660.0, 0.25, WINDOW))
            .map(|(a, b)| a + b)
            .collect();
        let level = |frequency| goertzel(mixed.iter().copied(), mixed.len(), frequency);
        assert!((level(440.0) - 0.5).abs() < 0.01, "{}", level(440.0));
        assert!((level(660.0) - 0.25).abs() < 0.01, "{}", level(660.0));
        assert!(level(550.0) < 0.01, "{}", level(550.0));
    }
}
