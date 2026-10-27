//! Recording (D7, §5.6): what this client receives of a room, written as it comes into WebM files,
//! one a participant, its camera's VP9 and its voice's Opus as they were sent, decrypted and
//! confirmed, never decoded and encoded again. moq-mux's Matroska exporter writes them: each
//! participant's frames go onto a broadcast of its own, in this process only, with a hang
//! catalog naming the two, and the exporter reads that broadcast as it would any.
//!
//! While a participant records, its signed profile says so, and every client shows who records
//! (an indicator, as Teams has, with no consent asked). The indicator relies on honest clients: a
//! modified client, or a screen recorder, records silently.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use crate::roster::ParticipantKey;
use crate::session::Error;

/// A frame for a recording: the time it's shown at, on its track's own timeline, and its
/// payload as the codec made it.
pub(crate) struct Frame {
    pub timestamp: moq_net::Timestamp,
    pub payload: Bytes,
    /// Whether it starts a group, and so decodes on its own.
    pub keyframe: bool,
}

/// Which of a participant's tracks a frame is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Camera,
    Voice,
}

/// Where a recording takes its frames from: the players of the others' tracks and the senders of
/// this participant's, which give it what they have while it records, and nothing else. Cheap
/// to clone; every clone is the same.
#[derive(Clone, Default)]
pub(crate) struct Tap(Arc<Mutex<Option<mpsc::UnboundedSender<Tapped>>>>);

/// What a track's sender or player gives a recording: whose track, and which.
#[derive(Clone)]
pub(crate) struct Tapping {
    pub tap: Tap,
    pub whose: ParticipantKey,
    pub kind: Kind,
}

impl Tapping {
    /// A frame, made only while it records.
    pub fn give(&self, frame: impl FnOnce() -> Frame) {
        self.tap.give(self.whose, self.kind, frame);
    }
}

/// A frame given to a recording.
pub(crate) struct Tapped {
    whose: ParticipantKey,
    kind: Kind,
    frame: Frame,
}

impl Tap {
    /// `whose` frame of `kind`, made only while it records.
    pub fn give(&self, whose: ParticipantKey, kind: Kind, frame: impl FnOnce() -> Frame) {
        if let Some(recording) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            // A recording that ended takes nothing more.
            let _ended: Result<(), _> = recording.send(Tapped {
                whose,
                kind,
                frame: frame(),
            });
        }
    }

    fn start(&self) -> mpsc::UnboundedReceiver<Tapped> {
        let (sender, frames) = mpsc::unbounded_channel();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        frames
    }

    fn stop(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// A recording under way: a file a participant, in a directory of its own.
pub(crate) struct Recording {
    tap: Tap,
    writing: tokio::task::JoinHandle<()>,
}

impl Recording {
    /// Records what `tap` is given into a new directory in `recordings`, named for `room` and
    /// the time, each participant's file named as `names` calls them.
    pub fn start(
        tap: &Tap,
        recordings: &Path,
        room: &str,
        names: watch::Receiver<crate::RoomState>,
    ) -> Result<Self, Error> {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let directory = recordings.join(format!("{}-{started}", &room[..room.len().min(8)]));
        std::fs::create_dir_all(&directory)
            .map_err(|error| Error::Recording(format!("{}: {error}", directory.display())))?;
        tracing::info!(directory = %directory.display(), "recording");
        let frames = tap.start();
        let writing = tokio::spawn(record(frames, directory.clone(), names));
        Ok(Self {
            tap: tap.clone(),
            writing,
        })
    }

    /// Stops recording, once every file is written out.
    pub async fn stop(self) {
        self.tap.stop();
        if let Err(error) = self.writing.await {
            tracing::warn!(%error, "a recording's writer stopped");
        }
    }
}

/// Writes what `frames` brings into `directory`, a file a participant, made with its first frame,
/// until the tap stops; then finishes each.
async fn record(
    mut frames: mpsc::UnboundedReceiver<Tapped>,
    directory: PathBuf,
    names: watch::Receiver<crate::RoomState>,
) {
    let mut files: HashMap<ParticipantKey, Option<File>> = HashMap::new();
    while let Some(Tapped { whose, kind, frame }) = frames.recv().await {
        if let std::collections::hash_map::Entry::Vacant(vacant) = files.entry(whose) {
            let name = {
                let state = names.borrow();
                std::iter::once(&state.me)
                    .chain(&state.participants)
                    .find(|participant| participant.key == whose)
                    .map(|participant| participant.profile.name.clone())
                    .unwrap_or_default()
            };
            let path = path(&directory, &name, whose);
            // One that can't be made is left out; the others go on.
            let file = File::create(path.clone())
                .await
                .inspect_err(|error| tracing::warn!(%error, path = %path.display(), "a recording"))
                .ok();
            vacant.insert(file);
        }
        if let Some(Some(file)) = files.get_mut(&whose) {
            file.write(kind, frame);
        }
    }
    for file in files.into_values().flatten() {
        file.finish().await;
    }
}

/// One participant's recording: a file being written, from a broadcast of this process's own.
pub(crate) struct File {
    camera: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
    voice: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
    /// Keeps the broadcast up, and with it the exporter's subscriptions.
    _broadcast: moq_net::broadcast::Producer,
    _origin: moq_net::origin::Producer,
    _catalog: moq_mux::catalog::Producer,
    writing: tokio::task::JoinHandle<()>,
    /// Whether the camera's first keyframe came: a recording begun mid-group starts at the next.
    keyed: bool,
}

impl File {
    /// Starts writing `path`, from frames given with [`Self::write`], until it's dropped.
    pub async fn create(path: PathBuf) -> Result<Self, Error> {
        let local = |error: moq_net::Error| Error::Recording(error.to_string());
        let mux = |error: moq_mux::Error| Error::Recording(error.to_string());
        // An origin of this process's own, which the exporter finds the broadcast in.
        let origin = moq_tokio::origin::spawn();
        let mut broadcast = origin.create_broadcast(LOCAL).map_err(local)?;
        let consumer = broadcast.consume();
        let mut catalog =
            moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default())
                .map_err(local)?;
        let camera_track = broadcast
            .create_track(
                broadcast.unique_name(".camera"),
                hang::container::track_info(hang::catalog::PRIORITY.video),
            )
            .map_err(local)?;
        let voice_track = broadcast
            .create_track(
                broadcast.unique_name(".voice"),
                hang::container::track_info(hang::catalog::PRIORITY.audio),
            )
            .map_err(local)?;
        {
            let settings = crate::video::CAMERA_SETTINGS;
            let mut video = hang::catalog::VideoConfig::new(hang::catalog::VP9 {
                profile: 0,
                level: 31,
                bit_depth: 8,
                chroma_subsampling: 1,
                color_primaries: 1,
                transfer_characteristics: 1,
                // BT.601, as every picture of mic's is.
                matrix_coefficients: 6,
                full_range: false,
            });
            video.coded_width = Some(settings.width);
            video.coded_height = Some(settings.height);
            video.framerate = Some(f64::from(settings.fps));
            video.container = hang::catalog::Container::Legacy;
            let mut audio =
                hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 1);
            audio.container = hang::catalog::Container::Legacy;
            let mut modified = catalog.modify().map_err(mux)?;
            modified
                .video
                .renditions
                .insert(camera_track.name().to_owned(), video);
            modified
                .audio
                .renditions
                .insert(voice_track.name().to_owned(), audio);
            modified.commit().map_err(mux)?;
        }
        // Announced, as the exporter finds broadcasts.
        broadcast.announce(Default::default()).map_err(local)?;
        let legacy = |kind| moq_mux::catalog::hang::Container::Legacy(kind);
        let camera = moq_mux::container::Producer::new(
            camera_track,
            legacy(moq_mux::container::Kind::Video),
        );
        let voice =
            moq_mux::container::Producer::new(voice_track, legacy(moq_mux::container::Kind::Audio));
        let catalogs =
            moq_mux::catalog::Consumer::<()>::new(&consumer, moq_mux::catalog::CatalogFormat::Hang)
                .await
                .map_err(mux)?;
        // Every frame, not just the live edge: a recording is complete.
        let exporter = moq_mux::container::mkv::Export::new(
            moq_mux::Source::new(origin.consume(), LOCAL),
            catalogs,
        )
        .with_max_age(RECORDING_MAX_AGE);
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|error| Error::Recording(format!("{}: {error}", path.display())))?;
        let writing = tokio::spawn(async move {
            let mut exporter = exporter;
            loop {
                match exporter.next().await {
                    Ok(Some(chunk)) => {
                        if let Err(error) = file.write_all(&chunk).await {
                            tracing::warn!(%error, path = %path.display(), "writing a recording");
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "a recording's export");
                        break;
                    }
                }
            }
            if let Err(error) = file.flush().await {
                tracing::warn!(%error, path = %path.display(), "finishing a recording");
            }
        });
        Ok(Self {
            camera,
            voice,
            _broadcast: broadcast,
            _origin: origin,
            _catalog: catalog,
            writing,
            keyed: false,
        })
    }

    /// `frame` of its participant's `kind` of track, into the file.
    pub fn write(&mut self, kind: Kind, frame: Frame) {
        let track = match kind {
            Kind::Camera => {
                self.keyed |= frame.keyframe;
                if !self.keyed {
                    return;
                }
                &mut self.camera
            }
            Kind::Voice => &mut self.voice,
        };
        let written = track.write(moq_mux::container::Frame {
            timestamp: frame.timestamp,
            payload: frame.payload,
            keyframe: frame.keyframe,
            duration: None,
        });
        if let Err(error) = written {
            tracing::debug!(%error, ?kind, "a frame the recording didn't take");
        }
    }

    /// Ends the file: its tracks end, and what's left is written out, for up to a second.
    pub async fn finish(self) {
        let Self {
            mut camera,
            mut voice,
            _broadcast,
            _origin,
            _catalog,
            writing,
            keyed: _,
        } = self;
        let mut catalog = _catalog;
        for track in [&mut camera, &mut voice] {
            if let Err(error) = track.finish() {
                tracing::debug!(%error, "ending a recording's track");
            }
        }
        if let Err(error) = catalog.finish() {
            tracing::debug!(%error, "ending a recording's catalog");
        }
        // The exporter writes out what it holds, and ends with the broadcast.
        if tokio::time::timeout(std::time::Duration::from_secs(1), writing)
            .await
            .is_err()
        {
            tracing::warn!("a recording didn't finish writing within a second");
        }
        _broadcast.close();
    }
}

/// The recording's broadcast's path in its origin.
const LOCAL: &str = "recording";

/// How far behind a recording's exporter may fall before it skips: well past what a busy disk
/// takes, as a recording wants every frame.
const RECORDING_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

/// Where a participant's recording goes in `directory`: by name and the start of its key, so two
/// of one name are two files.
pub(crate) fn path(directory: &Path, name: &str, key: ParticipantKey) -> PathBuf {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    directory.join(format!("{name}-{}.webm", &key.to_string()[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mic_media::video::vp9::Vp9Encoder;
    use mic_media::video::{Encoder as _, Pattern};

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recording_is_webm_with_the_camera_and_the_voice() {
        let directory = std::env::temp_dir().join(format!("mic-recording-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("alice.webm");
        let mut file = File::create(path.clone()).await.unwrap();
        // As a room's frames come: after the exporter has subscribed.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let settings = crate::video::CAMERA_SETTINGS;
        let mut camera = Vp9Encoder::new(settings).unwrap();
        let mut voice = moq_audio::encode::Encoder::new(&crate::audio::settings()).unwrap();
        let fps = u64::from(settings.fps);
        for frame in 0..2 * fps {
            let picture = Pattern {
                sender: 1,
                frame: frame as u16,
            }
            .draw(settings.width, settings.height);
            for packet in camera.encode(&picture, frame % fps == 0).unwrap() {
                file.write(
                    Kind::Camera,
                    Frame {
                        timestamp: moq_net::Timestamp::from_micros(frame * 1_000_000 / fps)
                            .unwrap(),
                        payload: packet.data.into(),
                        keyframe: packet.keyframe,
                    },
                );
            }
        }
        let tone: Vec<f32> = (0..mic_media::audio::FRAME)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5)
            .collect();
        for packet in 0..100u64 {
            let encoded = voice.encode(&tone).unwrap();
            file.write(
                Kind::Voice,
                Frame {
                    timestamp: moq_net::Timestamp::from_micros(packet * 20_000).unwrap(),
                    payload: encoded.payload,
                    keyframe: true,
                },
            );
        }
        file.finish().await;
        let written = std::fs::read(&path).unwrap();
        assert_eq!(&written[..4], &[0x1a, 0x45, 0xdf, 0xa3], "an EBML file");
        for codec in [&b"V_VP9"[..], b"A_OPUS", b"webm"] {
            assert!(
                written.windows(codec.len()).any(|window| window == codec),
                "{}",
                String::from_utf8_lossy(codec)
            );
        }
        assert!(written.len() > 10_000, "{} bytes", written.len());
        // Where ffprobe is, it reads the file as two streams, and the camera's frames all.
        if let Ok(probed) = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,nb_read_frames",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
        {
            let said = String::from_utf8_lossy(&probed.stdout);
            assert!(
                probed.status.success(),
                "ffprobe: {}",
                String::from_utf8_lossy(&probed.stderr)
            );
            assert!(said.contains("vp9,30"), "{said}");
            assert!(said.contains("opus,"), "{said}");
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
