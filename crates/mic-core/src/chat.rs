//! Chat and reactions (D7, §5.6): a `chat` and a `reactions` track in every participant's
//! broadcast, encrypted and signed like every other (D17, D18), a group a message, its one frame
//! JSON. A reaction is one of a few, by name, so a frame can't carry anything else; a message is
//! text, cut to [`TEXT_LIMIT`] characters.
//!
//! Each message carries its sender's count of them, from 0 in each broadcast, so one read twice,
//! as a subscriber reads the newest group again when it subscribes again, is shown once. A
//! subscriber joining sees the last message sent before it did, the one the relay still has.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::roster::ParticipantKey;
use crate::sig::{SignedTrack, Verification};
use crate::tracks::{Peer, TrackError};

/// The chat's track's semantic name.
pub const CHAT: &str = "chat";
/// The reactions' track's semantic name.
pub const REACTIONS: &str = "reactions";
/// How long a message may be, in characters.
pub const TEXT_LIMIT: usize = 2000;

/// A reaction: one of these, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reaction {
    ThumbsUp,
    Clap,
    Laugh,
    Surprised,
    Heart,
    Party,
}

impl Reaction {
    pub const ALL: [Reaction; 6] = [
        Reaction::ThumbsUp,
        Reaction::Clap,
        Reaction::Laugh,
        Reaction::Surprised,
        Reaction::Heart,
        Reaction::Party,
    ];

    /// Its name, as a frame says it.
    pub fn name(self) -> &'static str {
        match self {
            Reaction::ThumbsUp => "thumbs_up",
            Reaction::Clap => "clap",
            Reaction::Laugh => "laugh",
            Reaction::Surprised => "surprised",
            Reaction::Heart => "heart",
            Reaction::Party => "party",
        }
    }

    /// How it's shown.
    pub fn emoji(self) -> &'static str {
        match self {
            Reaction::ThumbsUp => "👍",
            Reaction::Clap => "👏",
            Reaction::Laugh => "😂",
            Reaction::Surprised => "😮",
            Reaction::Heart => "❤️",
            Reaction::Party => "🎉",
        }
    }
}

/// Something said in the room: a message, or a reaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Said {
    Message { text: String },
    Reaction { reaction: Reaction },
}

/// A frame of either track: what's said, and the sender's count of what it said on that track.
#[derive(Serialize, Deserialize)]
struct Frame {
    n: u64,
    #[serde(flatten)]
    said: Said,
}

impl Frame {
    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a frame serializes")
    }

    /// What a frame says; none if it isn't one, or says what its track doesn't carry.
    fn decode(track: &str, bytes: &[u8]) -> Option<Self> {
        let mut frame: Self = serde_json::from_slice(bytes).ok()?;
        match (&mut frame.said, track) {
            (Said::Message { text }, CHAT) => *text = trimmed(text),
            (Said::Reaction { .. }, REACTIONS) => {}
            _ => return None,
        }
        Some(frame)
    }
}

/// `text`, its ends trimmed, cut to [`TEXT_LIMIT`] characters.
pub fn trimmed(text: &str) -> String {
    text.trim().chars().take(TEXT_LIMIT).collect()
}

/// This participant's chat and reactions tracks, and its counts of what it said on each.
pub(crate) struct Saying {
    chat: SignedTrack,
    reactions: SignedTrack,
    said: [u64; 2],
}

impl Saying {
    pub fn new(chat: SignedTrack, reactions: SignedTrack) -> Self {
        Self {
            chat,
            reactions,
            said: [0; 2],
        }
    }

    /// Says `said` on its track, as that track's latest group.
    pub fn say(&mut self, said: Said) -> Result<(), TrackError> {
        let (track, n) = match &said {
            Said::Message { .. } => (&mut self.chat, &mut self.said[0]),
            Said::Reaction { .. } => (&mut self.reactions, &mut self.said[1]),
        };
        let frame = Frame { n: *n, said };
        let mut group = track.append_group()?;
        group.write_frame(moq_net::Timestamp::now(), &frame.encode())?;
        group.finish()?;
        *n += 1;
        Ok(())
    }

    /// Ends both tracks.
    pub fn finish(self) {
        for track in [self.chat, self.reactions] {
            if let Err(error) = track.finish() {
                tracing::debug!(%error, "finishing a chat track");
            }
        }
    }
}

/// Something someone said, as a reader of their broadcast heard it.
#[derive(Debug)]
pub(crate) struct Heard {
    /// The broadcast's path: with `n`, what tells something read twice.
    pub path: String,
    pub from: ParticipantKey,
    pub n: u64,
    pub said: Said,
}

/// Reads `track` (chat or reactions) of `peer`, `whose` it is, into `heard`, its frames
/// confirmed as any others, until it ends or nobody hears any more.
pub(crate) async fn read(
    peer: Peer,
    track: &'static str,
    verification: Verification,
    whose: ParticipantKey,
    heard: mpsc::UnboundedSender<Heard>,
) {
    let mut reading = match peer.verified_track(track, &verification).await {
        Ok(reading) => reading,
        Err(error) => {
            tracing::debug!(%error, path = peer.path(), track, "no chat track");
            return;
        }
    };
    loop {
        let mut group = match reading.recv_group().await {
            Ok(Some(group)) => group,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, path = peer.path(), track, "a chat track failed");
                return;
            }
        };
        let Ok(Some(frame)) = group.read_frame().await else {
            continue;
        };
        let Some(frame) = Frame::decode(track, &frame.plaintext) else {
            tracing::debug!(path = peer.path(), track, "a frame that says nothing");
            continue;
        };
        let said = Heard {
            path: peer.path().to_owned(),
            from: whose,
            n: frame.n,
            said: frame.said,
        };
        if heard.send(said).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_on_its_own_track_only() {
        let message = Frame {
            n: 3,
            said: Said::Message {
                text: "hi".to_owned(),
            },
        };
        let bytes = message.encode();
        let read = Frame::decode(CHAT, &bytes).unwrap();
        assert_eq!((read.n, read.said), (3, message.said));
        assert!(Frame::decode(REACTIONS, &bytes).is_none());
        let reaction = Frame {
            n: 0,
            said: Said::Reaction {
                reaction: Reaction::Heart,
            },
        };
        assert!(Frame::decode(REACTIONS, &reaction.encode()).is_some());
        assert!(
            Frame::decode(
                REACTIONS,
                br#"{"n": 0, "kind": "reaction", "reaction": "poo"}"#
            )
            .is_none()
        );
    }

    #[test]
    fn a_reactions_name_is_how_a_frame_says_it() {
        for reaction in Reaction::ALL {
            assert_eq!(
                serde_json::to_value(reaction).unwrap(),
                serde_json::Value::from(reaction.name())
            );
        }
    }

    #[test]
    fn a_message_is_trimmed_and_cut() {
        let long = format!("  {}  ", "x".repeat(TEXT_LIMIT + 10));
        let bytes = serde_json::to_vec(&serde_json::json!({
            "n": 0, "kind": "message", "text": long
        }))
        .unwrap();
        let Some(Frame {
            said: Said::Message { text },
            ..
        }) = Frame::decode(CHAT, &bytes)
        else {
            panic!("no message");
        };
        assert_eq!(text.chars().count(), TEXT_LIMIT);
    }
}
