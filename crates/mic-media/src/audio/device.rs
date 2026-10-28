//! The system's microphones and speakers, through cpal (CoreAudio, WASAPI, ALSA, and PipeWire
//! through ALSA): the default ones, or one chosen by its id, as [`microphones`] and [`speakers`]
//! list them.
//!
//! Each stream lives on a thread of its own, which builds it, plays it, and drops it when its
//! [`Running`] is dropped: a cpal stream may not move between threads on every platform. Both run
//! at 48 kHz in `f32`; a microphone with more channels than one is mixed down, and a speaker's
//! channels each get the mono mix. A device that can't do 48 kHz `f32` is refused for now:
//! resampling comes when a platform needs it **(unverified beyond PipeWire)**.

use std::sync::{Arc, Mutex, PoisonError, mpsc};

use cpal::traits::{DeviceTrait as _, HostTrait as _};

use super::{Error, Mixer, Running, SAMPLE_RATE, Sink, Source, Writer};

/// A device there is, as a person picks it: its id, which picks it again later (what a settings
/// file keeps), and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// The microphones there are, the default first.
pub fn microphones() -> Vec<Device> {
    let host = cpal::default_host();
    let default = host
        .default_input_device()
        .and_then(|device| device.id().ok());
    listed(host.input_devices().ok(), default)
        .into_iter()
        .filter(|device| pipewire_kind(&device.id).is_none_or(|kind| kind == Some(Kind::Input)))
        .collect()
}

/// The speakers there are, the default first.
pub fn speakers() -> Vec<Device> {
    let host = cpal::default_host();
    let default = host
        .default_output_device()
        .and_then(|device| device.id().ok());
    listed(host.output_devices().ok(), default)
        .into_iter()
        .filter(|device| pipewire_kind(&device.id).is_none_or(|kind| kind == Some(Kind::Output)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Input,
    Output,
}

/// Which way a PipeWire device goes, by its id, if it's one: PipeWire offers every sink's monitor
/// as an input too, and a card's input and output by one name, which a person picking a
/// microphone shouldn't have to tell apart. The defaults are an input and an output; the sinks'
/// default, a monitor, is neither.
fn pipewire_kind(id: &str) -> Option<Option<Kind>> {
    let node = id.strip_prefix("pipewire:")?;
    Some(match node {
        "input_default" => Some(Kind::Input),
        "output_default" => Some(Kind::Output),
        node if node.contains("_input") => Some(Kind::Input),
        node if node.contains("_output") => Some(Kind::Output),
        _ => None,
    })
}

fn listed(
    devices: Option<impl Iterator<Item = cpal::Device>>,
    default: Option<cpal::DeviceId>,
) -> Vec<Device> {
    let mut listed: Vec<(bool, Device)> = devices
        .into_iter()
        .flatten()
        .filter_map(|device| {
            let id = device.id().ok()?;
            let name = match id.to_string().as_str() {
                "pipewire:input_default" | "pipewire:output_default" => "System default".to_owned(),
                _ => device
                    .description()
                    .map(|description| description.name().to_owned())
                    .unwrap_or_else(|_| id.to_string()),
            };
            Some((
                default.as_ref() == Some(&id),
                Device {
                    id: id.to_string(),
                    name,
                },
            ))
        })
        .collect();
    listed.sort_by_key(|(default, _)| !*default);
    listed.into_iter().map(|(_, device)| device).collect()
}

/// The device `id` names, or the default if there's none or it's gone.
fn chosen(
    id: Option<&str>,
    default: impl FnOnce(&cpal::Host) -> Option<cpal::Device>,
) -> Option<cpal::Device> {
    let host = cpal::default_host();
    id.and_then(|id| id.parse::<cpal::DeviceId>().ok())
        .and_then(|id| host.device_by_id(&id))
        .or_else(|| default(&host))
}

/// Where a microphone writes, kept across microphones: a microphone switched for another goes on
/// writing into the same capture, which its reader never sees end. Cheap to clone.
#[derive(Clone)]
pub struct SharedWriter(Arc<Mutex<Writer>>);

impl SharedWriter {
    pub fn new(writer: Writer) -> Self {
        Self(Arc::new(Mutex::new(writer)))
    }

    fn write(&self, samples: &[f32]) {
        // Uncontended but when a microphone takes over from another.
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(samples);
    }
}

/// A microphone, the default or the one an id names, with its echo cancelled against what a
/// mixer plays, if it's given one ([`Mixer::echo_canceller`](super::Mixer::echo_canceller)), and
/// how loud it is told to `level`, if anything listens.
#[derive(Default)]
pub struct Microphone {
    echo: Option<super::EchoCanceller>,
    device: Option<String>,
    level: Option<Level>,
}

/// How loud a microphone is now, for a meter: its last chunk's RMS, 0 to 1. Cheap to clone.
#[derive(Clone, Default)]
pub struct Level(Arc<std::sync::atomic::AtomicU32>);

impl Level {
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn set(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let rms = (samples.iter().map(|sample| sample * sample).sum::<f32>()
            / samples.len() as f32)
            .sqrt();
        self.0
            .store(rms.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

impl Microphone {
    pub fn new() -> Self {
        Self::default()
    }

    /// The microphone, with `echo` cancelling what the speaker plays out of it.
    pub fn cancelling(echo: super::EchoCanceller) -> Self {
        Self {
            echo: Some(echo),
            ..Self::default()
        }
    }

    /// The microphone `id` names ([`microphones`]), rather than the default.
    pub fn device(self, id: Option<String>) -> Self {
        Self { device: id, ..self }
    }

    /// Its loudness told to `level`.
    pub fn level(self, level: Level) -> Self {
        Self {
            level: Some(level),
            ..self
        }
    }

    /// Starts it writing into `samples`, which outlives it: another may take over.
    pub fn start_shared(self, samples: SharedWriter) -> Result<Running, Error> {
        let (echo, id, level) = (self.echo, self.device, self.level);
        if let Some(echo) = &echo {
            // What played while no microphone ran is of no use to the model now.
            echo.idle();
        }
        stream("mic-microphone", move || {
            let device = chosen(id.as_deref(), cpal::Host::default_input_device)
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
                            if let Some(level) = &level {
                                level.set(&mono[..count]);
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

/// A speaker, the default or the one an id names ([`speakers`]).
#[derive(Default)]
pub struct Speaker {
    device: Option<String>,
}

impl Speaker {
    /// The speaker `id` names, rather than the default.
    pub fn device(id: Option<String>) -> Self {
        Self { device: id }
    }
}

/// Samples a capture callback mixes down at a time, without allocating.
const CHUNK: usize = 480;

impl Source for Microphone {
    fn start(self: Box<Self>, samples: Writer) -> Result<Running, Error> {
        (*self).start_shared(SharedWriter::new(samples))
    }
}

impl Sink for Speaker {
    fn start(self: Box<Self>, mixer: Mixer) -> Result<Running, Error> {
        let id = self.device;
        stream("mic-speaker", move || {
            let device = chosen(id.as_deref(), cpal::Host::default_output_device)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipewires_monitors_are_no_microphones_nor_its_inputs_speakers() {
        let kind = |id| pipewire_kind(id);
        assert_eq!(kind("pipewire:input_default"), Some(Some(Kind::Input)));
        assert_eq!(
            kind("pipewire:alsa_input.pci-0000.analog-stereo"),
            Some(Some(Kind::Input))
        );
        assert_eq!(
            kind("pipewire:bluez_output.41_42.1"),
            Some(Some(Kind::Output))
        );
        assert_eq!(kind("pipewire:sink_default"), Some(None));
        // Not PipeWire's: whatever ALSA or another host lists.
        assert_eq!(kind("alsa:hw:1,0"), None);
    }
}
