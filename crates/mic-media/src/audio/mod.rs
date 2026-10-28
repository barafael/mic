//! Audio: mono PCM at 48 kHz, as `f32` in -1..1, which is what Opus takes and gives.
//!
//! A [`Source`] (a microphone, `Microphone` with the `devices` feature, or a [`ToneSource`]) runs on a thread of its own, as a device's
//! callback does, and writes samples into a [`Writer`]; the async side reads them a frame at a
//! time from the [`Capture`] at the ring's other end. A [`Sink`] (a speaker, `Speaker`,
//! or an [`AnalyzingSink`]) runs on a thread of its own too, and takes what it plays from a [`Mixer`],
//! which sums a [`Playout`] per stream the async side writes decoded audio into.
//!
//! Nothing on a source's or sink's thread waits for the async side: rings are lock-free, the
//! mixer only ever tries its lock, and the async side looks for samples on a timer instead of
//! being woken (a wake takes locks a real-time thread mustn't).

mod analyze;
#[cfg(feature = "devices")]
mod device;
#[cfg(feature = "devices")]
mod echo;
mod mixer;
pub mod noise;
mod ring;
mod tone;

use std::time::Duration;

use derive_more::{Display, Error};

pub use analyze::{Analyzer, AnalyzingSink, level};
#[cfg(feature = "devices")]
pub use device::{Device, Level, Microphone, SharedWriter, Speaker, microphones, speakers};
#[cfg(feature = "devices")]
pub use echo::EchoCanceller;
pub use mixer::{Mixer, Playout};
pub use ring::{Capture, Writer, capture};
pub use tone::ToneSource;

/// Samples a second.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples a frame: 20 ms, Opus's frame for speech.
pub const FRAME: usize = 960;
/// A frame's duration.
pub const FRAME_DURATION: Duration = Duration::from_millis(20);

/// `duration` in samples.
pub fn samples(duration: Duration) -> usize {
    (duration.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// A source's or sink's thread didn't start.
    #[display("an audio thread: {_0}")]
    Thread(std::io::Error),
    /// A device refused, or isn't there.
    #[display("the audio device: {_0}")]
    Device(#[error(not(source))] String),
}

/// Where audio comes from: a microphone, or something that plays one.
pub trait Source: Send + 'static {
    /// Starts writing samples into `samples`, from a thread of its own, until the handle it
    /// returns is dropped.
    fn start(self: Box<Self>, samples: Writer) -> Result<Running, Error>;
}

/// Where audio goes: a speaker, or something that listens like one.
pub trait Sink: Send + 'static {
    /// Starts playing what `mixer` gives, from a thread of its own, until the handle it returns
    /// is dropped.
    fn start(self: Box<Self>, mixer: Mixer) -> Result<Running, Error>;
}

/// A source or sink that runs: it stops when this is dropped.
#[must_use = "dropping it stops the source or sink"]
pub struct Running {
    stop: Box<dyn FnOnce() + Send>,
}

impl Running {
    /// A handle that runs `stop` when it's dropped.
    pub fn new(stop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            stop: Box::new(stop),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let stop = std::mem::replace(&mut self.stop, Box::new(|| {}));
        stop();
    }
}

/// A thread that does `step` every `period`, on a schedule of absolute deadlines so it doesn't
/// drift, as a device's callbacks come; it ends, and is joined, when the handle is dropped.
pub(crate) fn paced(
    name: &str,
    period: Duration,
    mut step: impl FnMut() + Send + 'static,
) -> Result<Running, Error> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let stopped = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn({
            let stopped = stopped.clone();
            move || {
                let mut next = std::time::Instant::now();
                while !stopped.load(Ordering::Acquire) {
                    step();
                    next += period;
                    let now = std::time::Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        // Behind (a loaded machine): catch up from now, as a device would.
                        next = now;
                    }
                }
            }
        })
        .map_err(Error::Thread)?;
    Ok(Running::new(move || {
        stopped.store(true, Ordering::Release);
        // A panicked thread has stopped all the same.
        drop(thread.join());
    }))
}
