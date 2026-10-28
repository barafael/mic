//! Recording (D7, §5.6): the call as this client has it, written into one WebM file, as a
//! recording of a call is watched. Its picture is composed fifteen times a second from what the
//! client decodes anyway: a shared screen, if anyone shares one; else the active speaker, while
//! someone speaks and for a moment after; else a grid of everyone, up to nine. Its sound is
//! every voice mixed, this participant's own among them. Both are encoded again, VP9 and Opus,
//! and written by moq-mux's Matroska exporter, which reads a broadcast of this process's own with
//! a hang catalog of the two.
//!
//! While a participant records, its signed profile says so, and every client shows who records
//! (an indicator, as Teams has, with no consent asked). The indicator relies on honest clients: a
//! modified client, or a screen recorder, records silently.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use mic_media::video::compose::{Rect, grid};
use mic_media::video::vp9::{Content, Settings, Vp9Encoder};
use mic_media::video::{Encoder as _, Picture};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use crate::roster::ParticipantKey;
use crate::session::Error;

/// How a recording's picture is encoded.
pub const RECORDING_SETTINGS: Settings = Settings {
    width: 1280,
    height: 720,
    fps: 15,
    kbps: 1500,
    content: Content::Camera,
};
/// How many tiles the grid has at most.
const GRID: usize = 9;
/// How long after the last speech the picture stays on the speaker, before the grid.
const SPEAKER_HOLD: Duration = Duration::from_secs(3);
/// The space between the grid's tiles, and around them.
const GAP: u32 = 8;

/// A frame for a recording: the time it's shown at, on its track's own timeline, and its
/// payload as the codec made it.
pub(crate) struct Frame {
    pub timestamp: moq_net::Timestamp,
    pub payload: Bytes,
    /// Whether it decodes on its own.
    pub keyframe: bool,
}

/// Which of a recording's tracks a frame is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Camera,
    Voice,
}

/// Where a recording takes the voices from: their players, and this participant's sender,
/// which give it their packets while it records, and nothing else. Cheap to clone; every clone
/// is the same.
#[derive(Clone, Default)]
pub(crate) struct Tap(Arc<Mutex<Option<mpsc::UnboundedSender<Tapped>>>>);

/// A voice's packet given to a recording.
pub(crate) struct Tapped {
    whose: ParticipantKey,
    packet: Bytes,
}

impl Tap {
    /// `whose` voice's packet, as Opus, made only while it records.
    pub fn give(&self, whose: ParticipantKey, packet: impl FnOnce() -> Bytes) {
        if let Some(recording) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            // A recording that ended takes nothing more.
            let _ended: Result<(), _> = recording.send(Tapped {
                whose,
                packet: packet(),
            });
        }
    }

    fn start(&self) -> mpsc::UnboundedReceiver<Tapped> {
        let (sender, packets) = mpsc::unbounded_channel();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        packets
    }

    fn stop(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// What a recording's picture is made of: the pictures the client decodes.
#[derive(Clone)]
pub(crate) struct Sources {
    /// The others' cameras, and their screens.
    pub board: crate::video::Board,
    pub screens: crate::video::Board,
    /// This participant's camera, if it sends one.
    pub preview: Option<crate::video::Pictures>,
}

/// A recording under way.
pub(crate) struct Recording {
    tap: Tap,
    stop: watch::Sender<bool>,
    mixing: tokio::task::JoinHandle<()>,
    composing: tokio::task::JoinHandle<()>,
    writing: tokio::task::JoinHandle<()>,
}

impl Recording {
    /// Records the call into a new file in `recordings`, named for `room` and the time: the
    /// voices `tap` is given, and a picture of `sources` as `state` has the room.
    pub async fn start(
        tap: &Tap,
        recordings: &Path,
        room: &str,
        state: watch::Receiver<crate::RoomState>,
        sources: Sources,
    ) -> Result<Self, Error> {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        std::fs::create_dir_all(recordings)
            .map_err(|error| Error::Recording(format!("{}: {error}", recordings.display())))?;
        // Named for the room, as its invite calls it, or by its id.
        let called = state
            .borrow()
            .name
            .clone()
            .map(|name| {
                name.chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>()
            })
            .unwrap_or_else(|| room[..room.len().min(8)].to_owned());
        let path = recordings.join(format!("{called}-{started}.webm"));
        tracing::info!(path = %path.display(), "recording");
        let file = File::create(path, RECORDING_SETTINGS).await?;
        let (frames, written) = mpsc::unbounded_channel();
        let (stop, stopped) = watch::channel(false);
        let voices = crate::speaker::Voices::default();
        let mixing = tokio::spawn(mix(
            tap.start(),
            voices.clone(),
            frames.clone(),
            stopped.clone(),
        ));
        let composing = tokio::spawn(compose(state, sources, voices, frames, stopped));
        let writing = tokio::spawn(write(file, written));
        Ok(Self {
            tap: tap.clone(),
            stop,
            mixing,
            composing,
            writing,
        })
    }

    /// Stops recording, once the file is written out.
    pub async fn stop(self) {
        self.tap.stop();
        self.stop.send_replace(true);
        for task in [self.mixing, self.composing, self.writing] {
            if let Err(error) = task.await {
                tracing::warn!(%error, "a recording's task stopped");
            }
        }
    }
}

/// Writes what `frames` brings into `file` until the mixer and the composer are done; then
/// finishes it.
async fn write(mut file: File, mut frames: mpsc::UnboundedReceiver<(Kind, Frame)>) {
    while let Some((kind, frame)) = frames.recv().await {
        file.write(kind, frame);
    }
    file.finish().await;
}

/// Every voice `packets` brings, decoded and mixed, encoded again 20 ms at a time into `frames`,
/// and how loud each is into `voices`, until `stopped`.
async fn mix(
    mut packets: mpsc::UnboundedReceiver<Tapped>,
    voices: crate::speaker::Voices,
    frames: mpsc::UnboundedSender<(Kind, Frame)>,
    mut stopped: watch::Receiver<bool>,
) {
    let mixer = mic_media::audio::Mixer::new();
    let mut encoder = match moq_audio::encode::Encoder::new(&crate::audio::settings()) {
        Ok(encoder) => encoder,
        Err(error) => {
            tracing::warn!(%error, "no Opus encoder: the recording has no sound");
            return;
        }
    };
    let mut streams: HashMap<
        ParticipantKey,
        (moq_audio::decode::Decoder, mic_media::audio::Playout),
    > = HashMap::new();
    let mut ticks = tokio::time::interval(Duration::from_millis(20));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut out = vec![0.0; mic_media::audio::FRAME];
    let mut sent: u64 = 0;
    loop {
        tokio::select! {
            packet = packets.recv() => {
                let Some(Tapped { whose, packet }) = packet else {
                    break;
                };
                let stream = match streams.entry(whose) {
                    std::collections::hash_map::Entry::Occupied(stream) => stream.into_mut(),
                    std::collections::hash_map::Entry::Vacant(vacant) => {
                        let Ok(decoder) = crate::audio::decoder() else {
                            continue;
                        };
                        vacant.insert((decoder, mixer.stream()))
                    }
                };
                match stream.0.decode(&packet) {
                    Ok(decoded) => {
                        voices.heard(whose, &decoded.samples);
                        stream.1.write(&decoded.samples);
                    }
                    Err(error) => tracing::debug!(%error, "a voice the recording didn't decode"),
                }
            }
            _ = ticks.tick() => {
                mixer.fill(&mut out);
                match encoder.encode(&out) {
                    Ok(packet) => {
                        let Ok(timestamp) = moq_net::Timestamp::from_micros(sent * 20_000) else {
                            break;
                        };
                        let frame = Frame {
                            timestamp,
                            payload: Bytes::copy_from_slice(&packet.payload),
                            keyframe: true,
                        };
                        if frames.send((Kind::Voice, frame)).is_err() {
                            break;
                        }
                        sent += 1;
                    }
                    Err(error) => tracing::debug!(%error, "encoding the recording's sound"),
                }
            }
            _ = stopped.changed() => break,
        }
    }
}

/// What the recording shows now: a screen, the speaker, or the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    Screen(ParticipantKey),
    Speaker(ParticipantKey),
    Grid(Vec<ParticipantKey>),
}

/// What's shown of `state`, with `speaker` the active speaker if someone spoke within the hold,
/// and `has_picture` saying whose camera has a picture to show.
fn choose(
    state: &crate::RoomState,
    speaker: Option<ParticipantKey>,
    has_picture: impl Fn(ParticipantKey) -> bool,
) -> Shown {
    if let Some(sharing) = state.participants.iter().find(|them| them.profile.sharing) {
        return Shown::Screen(sharing.key);
    }
    if let Some(speaker) = speaker {
        return Shown::Speaker(speaker);
    }
    // Those with a picture first, then the rest, in the order they joined.
    let everyone: Vec<&crate::Participant> = std::iter::once(&state.me)
        .chain(&state.participants)
        .collect();
    let (pictured, rest): (Vec<_>, Vec<_>) = everyone
        .into_iter()
        .partition(|them| !them.profile.camera_off && has_picture(them.key));
    Shown::Grid(
        pictured
            .into_iter()
            .chain(rest)
            .take(GRID)
            .map(|them| them.key)
            .collect(),
    )
}

/// The recording's picture, composed of `sources` as `state` has the room, encoded into `frames`
/// fifteen times a second, until `stopped`.
async fn compose(
    state: watch::Receiver<crate::RoomState>,
    sources: Sources,
    voices: crate::speaker::Voices,
    frames: mpsc::UnboundedSender<(Kind, Frame)>,
    mut stopped: watch::Receiver<bool>,
) {
    let settings = RECORDING_SETTINGS;
    let mut encoder = match Vp9Encoder::new(settings) {
        Ok(encoder) => Some(encoder),
        Err(error) => {
            tracing::warn!(%error, "no VP9 encoder: the recording has no picture");
            return;
        }
    };
    let mut ticks = tokio::time::interval(Duration::from_secs(1) / settings.fps);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut speaker: Option<ParticipantKey> = None;
    let mut spoke: Option<Instant> = None;
    let mut sent: u64 = 0;
    loop {
        tokio::select! {
            _ = ticks.tick() => {}
            _ = stopped.changed() => break,
        }
        let now = Instant::now();
        let state = state.borrow().clone();
        let me = state.me.key;
        let everyone: Vec<ParticipantKey> = std::iter::once(me)
            .chain(state.participants.iter().map(|them| them.key))
            .collect();
        // Who's muted sends nothing; who speaks, this participant among them, is the speaker.
        if !voices.speaking(&everyone, now).is_empty() {
            spoke = Some(now);
        }
        speaker = voices.active(speaker, &everyone, now);
        let speaking = speaker.filter(|_| spoke.is_some_and(|spoke| now - spoke < SPEAKER_HOLD));
        let picture_of = |key: ParticipantKey| -> Option<std::sync::Arc<Picture>> {
            let camera_off = std::iter::once(&state.me)
                .chain(&state.participants)
                .any(|them| them.key == key && them.profile.camera_off);
            if camera_off {
                return None;
            }
            if key == me {
                sources.preview.as_ref()?.borrow().clone()
            } else {
                sources.board.pictures(key).borrow().clone()
            }
        };
        let shown = choose(&state, speaking, |key| picture_of(key).is_some());
        let mut canvas = Picture::black(settings.width, settings.height);
        let whole = Rect {
            x: 0,
            y: 0,
            width: settings.width,
            height: settings.height,
        };
        match &shown {
            Shown::Screen(whose) => {
                if let Some(screen) = sources.screens.pictures(*whose).borrow().clone() {
                    canvas.draw(&screen, whole);
                }
            }
            Shown::Speaker(whose) => match picture_of(*whose) {
                Some(camera) => canvas.draw(&camera, whole),
                None => canvas.fill(whole, card(*whose)),
            },
            Shown::Grid(keys) => {
                for (key, cell) in
                    keys.iter()
                        .zip(grid(keys.len(), settings.width, settings.height, GAP))
                {
                    match picture_of(*key) {
                        Some(camera) => canvas.draw(&camera, cell),
                        None => canvas.fill(cell, card(*key)),
                    }
                }
            }
        }
        let keyframe = sent.is_multiple_of(u64::from(settings.fps));
        let Some(mut working) = encoder.take() else {
            break;
        };
        let encoded = tokio::task::spawn_blocking(move || {
            let packets = working.encode(&canvas, keyframe);
            (working, packets)
        })
        .await;
        let packets = match encoded {
            Ok((working, packets)) => {
                encoder = Some(working);
                packets
            }
            Err(error) => {
                tracing::warn!(%error, "the recording's encoder stopped");
                break;
            }
        };
        let Ok(timestamp) =
            moq_net::Timestamp::from_micros(sent * 1_000_000 / u64::from(settings.fps))
        else {
            break;
        };
        match packets {
            Ok(packets) => {
                for (index, packet) in packets.into_iter().enumerate() {
                    let frame = Frame {
                        timestamp,
                        payload: packet.data.into(),
                        keyframe: keyframe && index == 0,
                    };
                    if frames.send((Kind::Camera, frame)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => tracing::debug!(%error, "encoding the recording's picture"),
        }
        sent += 1;
    }
}

/// What stands for `whose` without a picture: its colour, as Y, U and V.
fn card(whose: ParticipantKey) -> (u8, u8, u8) {
    let (u, v) = mic_media::video::tint(whose.as_bytes()[0]);
    (60, u, v)
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
    /// Starts writing `path`, its picture as `video` has it, from frames given with
    /// [`Self::write`], until it's finished.
    pub async fn create(path: PathBuf, video: Settings) -> Result<Self, Error> {
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
            let settings = video;
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

#[cfg(test)]
mod tests {
    use super::*;
    use mic_media::video::Pattern;
    use mic_media::video::vp9::Vp9Encoder;

    fn participant(byte: u8) -> crate::Participant {
        crate::Participant {
            key: ParticipantKey::from_bytes([byte; 32]),
            profile: crate::Profile::new("someone"),
            ..Default::default()
        }
    }

    fn state(others: u8) -> crate::RoomState {
        crate::RoomState {
            room: "room".to_owned(),
            me: participant(0),
            participants: (1..=others).map(participant).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_screen_then_the_speaker_then_the_grid() {
        let key = |byte| ParticipantKey::from_bytes([byte; 32]);
        let mut state = state(11);
        // Nobody speaks: the grid, those with a picture first, nine.
        let shown = choose(&state, None, |key| key.as_bytes()[0] >= 5);
        let Shown::Grid(keys) = shown else {
            panic!("{shown:?}");
        };
        assert_eq!(keys.len(), GRID);
        assert_eq!(keys[0], key(5));
        assert!(
            keys.contains(&key(0)),
            "this participant, if room's left: {keys:?}"
        );
        assert_eq!(
            choose(&state, Some(key(3)), |_| true),
            Shown::Speaker(key(3))
        );
        state.participants[6].profile.sharing = true;
        assert_eq!(
            choose(&state, Some(key(3)), |_| true),
            Shown::Screen(key(7))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recording_is_webm_with_the_camera_and_the_voice() {
        let directory = std::env::temp_dir().join(format!("mic-recording-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("alice.webm");
        let mut file = File::create(path.clone(), crate::video::CAMERA_SETTINGS)
            .await
            .unwrap();
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
