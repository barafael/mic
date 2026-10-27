//! A participant's camera: VP9 on a `camera` track of its broadcast, encrypted and signed like
//! every other (D17, D18), from a picture source; and every other participant's, decoded onto a
//! [`Board`], which a frontend's tiles read (`Room::video`).
//!
//! **Codecs.** A sender sends in the first codec of its preference that every other participant
//! decodes (mic-media's `choose`), named in its catalog's renditions. A receiver plays a camera
//! whose codec it decodes, and one it doesn't is a tile that says so (D10, §5.4). v1 has one codec.
//!
//! **Groups of a second**, each starting with a keyframe, so a subscriber coming in waits at
//! most a second for a picture it can decode, and the checkpoints list one or two groups a track
//! (as for the voice, `crate::audio`). Encoding and decoding block, for some milliseconds a
//! picture, so they run on the runtime's blocking threads, not its workers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use mic_media::video::vp9::{Content, Settings, Vp9Decoder, Vp9Encoder};
use mic_media::video::{Decoder as _, Encoder as _, Picture};
use tokio::sync::watch;

use crate::roster::ParticipantKey;
use crate::sig::{SignedTrack, VerifiedTrack};

/// The camera's track's semantic name.
pub const CAMERA: &str = "camera";

/// Pictures as a camera hands them over: the newest replaces the last (mic-media's
/// `PatternCamera`, or a device).
pub type Pictures = watch::Receiver<Option<Arc<Picture>>>;

/// How the camera is sent: small and light, as a call's tiles are, for now.
pub const CAMERA_SETTINGS: Settings = Settings {
    width: 640,
    height: 360,
    fps: 15,
    kbps: 500,
    content: Content::Camera,
};

/// What a participant sends as its camera.
#[derive(Clone)]
pub enum Camera {
    /// Pictures from a source, encoded as they come.
    Live(Pictures),
    /// Pictures encoded once, sent in a loop: what a crowd of many sends without the CPU of an
    /// encoder each (PLAN.md, commit 42).
    Recorded(Recorded),
}

/// A camera's pictures encoded once at [`CAMERA_SETTINGS`], a keyframe each second, to send in
/// a loop. Cheap to clone.
#[derive(Clone)]
pub struct Recorded {
    /// Each picture's packets, a whole number of seconds of them.
    pictures: Arc<Vec<Vec<Vec<u8>>>>,
}

impl Recorded {
    /// `pictures` encoded, as many as make whole seconds; none is an error.
    pub fn encode(pictures: impl IntoIterator<Item = Picture>) -> Result<Self, String> {
        let settings = CAMERA_SETTINGS;
        let fps = settings.fps as usize;
        let mut encoder = Vp9Encoder::new(settings).map_err(|error| error.to_string())?;
        let mut encoded = Vec::new();
        for (index, picture) in pictures.into_iter().enumerate() {
            let packets = encoder
                .encode(&picture, index.is_multiple_of(fps))
                .map_err(|error| error.to_string())?;
            encoded.push(packets.into_iter().map(|packet| packet.data).collect());
        }
        encoded.truncate(encoded.len() / fps * fps);
        if encoded.is_empty() {
            return Err(format!("under a second of pictures ({fps} make one)"));
        }
        Ok(Self {
            pictures: Arc::new(encoded),
        })
    }
}

/// Sends `camera` onto `track`, a group a second, each starting with a keyframe, until the
/// camera or the track end. While `on` says it's off, nothing is sent, and the next picture
/// after it's on again starts a group.
pub(crate) async fn publish(camera: Camera, track: SignedTrack, on: watch::Receiver<bool>) {
    match camera {
        Camera::Live(pictures) => publish_live(pictures, track, on).await,
        Camera::Recorded(recorded) => publish_recorded(recorded, track, on).await,
    }
}

async fn publish_live(mut pictures: Pictures, track: SignedTrack, on: watch::Receiver<bool>) {
    let settings = CAMERA_SETTINGS;
    let mut encoder = match Vp9Encoder::new(settings) {
        Ok(encoder) => Some(encoder),
        Err(error) => {
            tracing::warn!(%error, "no VP9 encoder: the camera isn't sent");
            return;
        }
    };
    let mut sending = Sending::new(track);
    while pictures.changed().await.is_ok() {
        let Some(picture) = pictures.borrow_and_update().clone() else {
            continue;
        };
        if !*on.borrow() {
            sending.pause();
            continue;
        }
        if (picture.width, picture.height) != (settings.width, settings.height) {
            tracing::debug!(
                width = picture.width,
                height = picture.height,
                "a picture the camera's encoder isn't sized for"
            );
            continue;
        }
        let keyframe = sending.keyframe_next();
        let Some(mut working) = encoder.take() else {
            return;
        };
        let encoded = tokio::task::spawn_blocking(move || {
            let packets = working.encode(&picture, keyframe);
            (working, packets)
        })
        .await;
        let packets = match encoded {
            Ok((working, packets)) => {
                encoder = Some(working);
                packets
            }
            Err(error) => {
                tracing::warn!(%error, "the camera's encoder stopped");
                return;
            }
        };
        match packets {
            Ok(packets) => {
                let data: Vec<Vec<u8>> = packets.into_iter().map(|packet| packet.data).collect();
                if !sending.send(&data) {
                    return;
                }
            }
            Err(error) => tracing::warn!(%error, "encoding the camera"),
        }
    }
}

/// `recorded`, from its start, at the camera's rate, over and over. Paused, it starts again
/// from its start, a keyframe.
async fn publish_recorded(recorded: Recorded, track: SignedTrack, on: watch::Receiver<bool>) {
    let mut ticks = tokio::time::interval(Duration::from_secs(1) / CAMERA_SETTINGS.fps);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sending = Sending::new(track);
    let mut next = 0;
    loop {
        ticks.tick().await;
        if !*on.borrow() {
            sending.pause();
            next = 0;
            continue;
        }
        if !sending.send(&recorded.pictures[next]) {
            return;
        }
        next = (next + 1) % recorded.pictures.len();
    }
}

/// A camera's pictures onto its track: a group a second, each starting with a keyframe, on the
/// camera's timeline.
struct Sending {
    track: SignedTrack,
    group: Option<crate::sig::SignedGroup>,
    /// Pictures sent, and skipped while paused: where the timeline is.
    sent: u64,
}

impl Sending {
    fn new(track: SignedTrack) -> Self {
        Self {
            track,
            group: None,
            sent: 0,
        }
    }

    fn fps() -> u64 {
        u64::from(CAMERA_SETTINGS.fps)
    }

    /// Whether the next picture starts a group, and so must be a keyframe.
    fn keyframe_next(&self) -> bool {
        self.sent.is_multiple_of(Self::fps())
    }

    /// Nothing sent for now: the next picture starts a group.
    fn pause(&mut self) {
        self.sent = self.sent.next_multiple_of(Self::fps());
    }

    /// The next picture's packets, encoded as [`Self::keyframe_next`] said; false once the track
    /// is over.
    fn send(&mut self, packets: &[Vec<u8>]) -> bool {
        if self.keyframe_next() {
            if let Some(full) = self.group.take()
                && let Err(error) = crate::sig::SignedGroup::finish(full)
            {
                tracing::debug!(%error, "finishing a camera group");
            }
            match self.track.append_group() {
                Ok(new) => self.group = Some(new),
                Err(error) => {
                    tracing::debug!(%error, "the camera track ended");
                    return false;
                }
            }
        }
        let timestamp = match moq_net::Timestamp::from_micros(self.sent * 1_000_000 / Self::fps()) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                tracing::warn!(%error, "the camera's timeline ran out");
                return false;
            }
        };
        if let Some(open) = self.group.as_mut() {
            for packet in packets {
                if let Err(error) = open.write_frame(timestamp, packet) {
                    tracing::debug!(%error, "the camera track ended");
                    return false;
                }
            }
        }
        self.sent += 1;
        true
    }
}

/// Decodes `track`, `whose` camera, onto `board`, until the track ends.
pub(crate) async fn play(mut track: VerifiedTrack, whose: ParticipantKey, board: Board) {
    let mut decoder = match Vp9Decoder::new() {
        Ok(decoder) => Some(decoder),
        Err(error) => {
            tracing::warn!(%error, "no VP9 decoder: a camera isn't shown");
            return;
        }
    };
    loop {
        let mut group = match track.recv_group().await {
            Ok(Some(group)) => group,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, "a camera track failed");
                return;
            }
        };
        loop {
            let frame = match group.read_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    tracing::debug!(%error, "a camera group failed");
                    break;
                }
            };
            let Some(mut working) = decoder.take() else {
                return;
            };
            let decoded = tokio::task::spawn_blocking(move || {
                let picture = working.decode(&frame.plaintext);
                (working, picture)
            })
            .await;
            match decoded {
                Ok((working, picture)) => {
                    decoder = Some(working);
                    match picture {
                        Ok(Some(picture)) => board.show(whose, Arc::new(picture)),
                        Ok(None) => {}
                        Err(error) => tracing::debug!(%error, "a camera packet didn't decode"),
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "a camera's decoder stopped");
                    return;
                }
            }
        }
    }
}

/// What every other participant's camera last showed, by key, and how many pictures came.
/// Cheap to clone; every clone is the same board.
#[derive(Clone, Default)]
pub struct Board {
    seen: Arc<Mutex<HashMap<ParticipantKey, Seen>>>,
}

struct Seen {
    pictures: watch::Sender<Option<Arc<Picture>>>,
    count: u64,
}

impl Seen {
    fn new() -> Self {
        Self {
            pictures: watch::channel(None).0,
            count: 0,
        }
    }
}

impl Board {
    fn show(&self, whose: ParticipantKey, picture: Arc<Picture>) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = seen.entry(whose).or_insert_with(Seen::new);
        entry.count += 1;
        entry.pictures.send_replace(Some(picture));
    }

    /// `whose` camera's pictures as they come: none until the first.
    pub fn pictures(&self, whose: ParticipantKey) -> Pictures {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(whose)
            .or_insert_with(Seen::new)
            .pictures
            .subscribe()
    }

    /// How many of `whose` camera's pictures were decoded.
    pub fn count(&self, whose: ParticipantKey) -> u64 {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&whose)
            .map_or(0, |seen| seen.count)
    }
}
