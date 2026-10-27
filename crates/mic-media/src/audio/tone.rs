//! A synthetic microphone: a sine at a frequency of its own, so a check knows whose audio it
//! hears (PLAN.md, M5: each client hears every other's frequency, never its own).

use std::f64::consts::TAU;
use std::time::Duration;

use super::{Error, Running, SAMPLE_RATE, Source, Writer, paced};

/// How often it writes: 10 ms, as a device's callback comes.
const PERIOD: Duration = Duration::from_millis(10);

/// A sine at `frequency` Hz, `amplitude` loud (0..1), written in real time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneSource {
    pub frequency: f64,
    pub amplitude: f32,
}

impl ToneSource {
    pub fn new(frequency: f64) -> Self {
        Self {
            frequency,
            amplitude: 0.5,
        }
    }

    /// The next `count` samples from `phase` (in cycles), and the phase after them.
    fn samples(&self, phase: f64, count: usize) -> (Vec<f32>, f64) {
        let step = self.frequency / f64::from(SAMPLE_RATE);
        let samples = (0..count)
            .map(|i| ((phase + step * i as f64) * TAU).sin() as f32 * self.amplitude)
            .collect();
        (samples, (phase + step * count as f64).fract())
    }
}

impl Source for ToneSource {
    fn start(self: Box<Self>, mut samples: Writer) -> Result<Running, Error> {
        let count = super::samples(PERIOD);
        let mut phase = 0.0;
        paced("mic-tone", PERIOD, move || {
            let (next, after) = self.samples(phase, count);
            phase = after;
            samples.write(&next);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_join_without_a_seam() {
        let tone = ToneSource::new(440.0);
        let (whole, _) = tone.samples(0.0, 1000);
        let (first, phase) = tone.samples(0.0, 300);
        let (rest, _) = tone.samples(phase, 700);
        let joined: Vec<f32> = first.into_iter().chain(rest).collect();
        for (a, b) in whole.iter().zip(&joined) {
            assert!((a - b).abs() < 1e-4, "{a} {b}");
        }
    }
}
