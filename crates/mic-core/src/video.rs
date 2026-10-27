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

/// Encodes what `pictures` gives onto `track`, a group a second, each starting with a keyframe,
/// until the pictures or the track end. While `on` says it's off, nothing is sent, and the next
/// picture after it's on again starts a group.
pub(crate) async fn publish(
    mut pictures: Pictures,
    mut track: SignedTrack,
    on: watch::Receiver<bool>,
) {
    let settings = CAMERA_SETTINGS;
    let mut encoder = match Vp9Encoder::new(settings) {
        Ok(encoder) => Some(encoder),
        Err(error) => {
            tracing::warn!(%error, "no VP9 encoder: the camera isn't sent");
            return;
        }
    };
    let mut group = None;
    let mut sent: u64 = 0;
    while pictures.changed().await.is_ok() {
        let Some(picture) = pictures.borrow_and_update().clone() else {
            continue;
        };
        if !*on.borrow() {
            // Back on, it starts with a keyframe.
            sent = sent.next_multiple_of(u64::from(settings.fps));
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
        let keyframe = sent.is_multiple_of(u64::from(settings.fps));
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
        let packets = match packets {
            Ok(packets) => packets,
            Err(error) => {
                tracing::warn!(%error, "encoding the camera");
                continue;
            }
        };
        if keyframe {
            if let Some(full) = group.take()
                && let Err(error) = crate::sig::SignedGroup::finish(full)
            {
                tracing::debug!(%error, "finishing a camera group");
            }
            match track.append_group() {
                Ok(new) => group = Some(new),
                Err(error) => {
                    tracing::debug!(%error, "the camera track ended");
                    return;
                }
            }
        }
        let timestamp =
            match moq_net::Timestamp::from_micros(sent * 1_000_000 / u64::from(settings.fps)) {
                Ok(timestamp) => timestamp,
                Err(error) => {
                    tracing::warn!(%error, "the camera's timeline ran out");
                    return;
                }
            };
        if let Some(open) = group.as_mut() {
            for packet in packets {
                if let Err(error) = open.write_frame(timestamp, &packet.data) {
                    tracing::debug!(%error, "the camera track ended");
                    return;
                }
            }
        }
        sent += 1;
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
