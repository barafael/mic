//! The system's default microphone and speaker, through cpal (CoreAudio, WASAPI, ALSA, and
//! PipeWire through ALSA).
//!
//! Each stream lives on a thread of its own, which builds it, plays it, and drops it when its
//! [`Running`] is dropped: a cpal stream may not move between threads on every platform. Both run
//! at 48 kHz in `f32`; a microphone with more channels than one is mixed down, and a speaker's
//! channels each get the mono mix. A device that can't do 48 kHz `f32` is refused for now:
//! resampling comes when a platform needs it **(unverified beyond PipeWire)**.

use std::sync::mpsc;

use cpal::traits::{DeviceTrait as _, HostTrait as _};

use super::{Error, Mixer, Running, SAMPLE_RATE, Sink, Source, Writer};

/// The default microphone, with its echo cancelled against what a mixer plays, if it's given
/// one ([`Mixer::echo_canceller`](super::Mixer::echo_canceller)).
#[derive(Default)]
pub struct Microphone {
    echo: Option<super::EchoCanceller>,
}

impl Microphone {
    pub fn new() -> Self {
        Self::default()
    }

    /// The microphone, with `echo` cancelling what the speaker plays out of it.
    pub fn cancelling(echo: super::EchoCanceller) -> Self {
        Self { echo: Some(echo) }
    }
}

/// The default speaker.
pub struct Speaker;

/// Samples a capture callback mixes down at a time, without allocating.
const CHUNK: usize = 480;

impl Source for Microphone {
    fn start(self: Box<Self>, mut samples: Writer) -> Result<Running, Error> {
        let echo = self.echo;
        if let Some(echo) = &echo {
            // What played while no microphone ran is of no use to the model now.
            echo.idle();
        }
        stream("mic-microphone", move || {
            let device = cpal::default_host()
                .default_input_device()
                .ok_or_else(|| Error::Device("no microphone".to_owned()))?;
            let config = config(device.supported_input_configs().map_err(device_error)?)?;
            let channels = usize::from(config.channels);
            let mut mono = [0.0_f32; CHUNK];
            device
                .build_input_stream::<f32, _, _>(
                    config,
                    move |data, _| {
                        for frames in data.chunks(CHUNK * channels) {
                            let count = frames.len() / channels;
                            for (sample, frame) in mono.iter_mut().zip(frames.chunks(channels)) {
                                *sample = frame.iter().sum::<f32>() / channels as f32;
                            }
                            if let Some(echo) = &echo {
                                echo.process(&mut mono[..count]);
                            }
                            samples.write(&mono[..count]);
                        }
                    },
                    |error| tracing::warn!(%error, "the microphone"),
                    None,
                )
                .map_err(device_error)
        })
    }
}

impl Sink for Speaker {
    fn start(self: Box<Self>, mixer: Mixer) -> Result<Running, Error> {
        stream("mic-speaker", move || {
            let device = cpal::default_host()
                .default_output_device()
                .ok_or_else(|| Error::Device("no speaker".to_owned()))?;
            let config = config(device.supported_output_configs().map_err(device_error)?)?;
            let channels = usize::from(config.channels);
            // Grown once if a callback asks for more than it has, then never again.
            let mut mono = vec![0.0_f32; 4096];
            device
                .build_output_stream::<f32, _, _>(
                    config,
                    move |data, _| {
                        let frames = data.len() / channels;
                        if mono.len() < frames {
                            mono.resize(frames, 0.0);
                        }
                        mixer.fill(&mut mono[..frames]);
                        for (frame, &sample) in data.chunks_mut(channels).zip(&mono) {
                            frame.fill(sample);
                        }
                    },
                    |error| tracing::warn!(%error, "the speaker"),
                    None,
                )
                .map_err(device_error)
        })
    }
}

fn device_error(error: impl std::fmt::Display) -> Error {
    Error::Device(error.to_string())
}

/// 48 kHz `f32`, with as few channels as the device has.
fn config(
    supported: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
) -> Result<cpal::StreamConfig, Error> {
    supported
        .filter(|range| {
            range.sample_format() == cpal::SampleFormat::F32
                && range.min_sample_rate() <= SAMPLE_RATE
                && SAMPLE_RATE <= range.max_sample_rate()
        })
        .min_by_key(|range| range.channels())
        .map(|range| cpal::StreamConfig {
            channels: range.channels(),
            sample_rate: SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Default,
        })
        .ok_or_else(|| Error::Device(format!("no {SAMPLE_RATE} Hz f32 configuration")))
}

/// A thread that builds a stream with `build`, plays it, and keeps it until the handle is
/// dropped; how the build went comes back before this returns.
fn stream<S: cpal::traits::StreamTrait>(
    name: &str,
    build: impl FnOnce() -> Result<S, Error> + Send + 'static,
) -> Result<Running, Error> {
    let (started, starting) = mpsc::sync_channel::<Result<(), Error>>(1);
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let stream = match build().and_then(|stream| {
                stream.play().map_err(device_error)?;
                Ok(stream)
            }) {
                Ok(stream) => stream,
                Err(error) => {
                    let _told: Result<(), _> = started.send(Err(error));
                    return;
                }
            };
            let _told: Result<(), _> = started.send(Ok(()));
            // Until the handle is dropped, which drops `stop`.
            let _stopped: Result<(), _> = stopped.recv();
            drop(stream);
        })
        .map_err(Error::Thread)?;
    starting
        .recv()
        .map_err(|_| Error::Device("the device's thread ended".to_owned()))??;
    Ok(Running::new(move || {
        drop(stop);
        drop(thread.join());
    }))
}
