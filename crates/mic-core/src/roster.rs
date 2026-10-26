//! Who is in the room (§1.5, §5.2): every participant publishes a `profile` track, encrypted like
//! every track, whose latest group is its profile; the roster comes from the broadcasts announced
//! under the room's prefix, and their profiles. A participant that leaves, or whose session ends,
//! is unannounced, and leaves the roster.
//!
//! The profile names the participant's key in the room. Until the signatures (D18, M3) bind it,
//! that's only what the profile says.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::tracks::{Peer, PeerEvent, Peers, TrackError};

/// The profile track's semantic name.
pub const PROFILE: &str = "profile";
/// How long a display name may be, in characters.
pub const NAME_LIMIT: usize = 64;

/// A participant's key in the room: what others know it by.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantKey([u8; 32]);

impl ParticipantKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ParticipantKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for ParticipantKey {
    /// The first 8 hex digits: enough to tell participants apart in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ParticipantKey({}…)", &self.to_string()[..8])
    }
}

/// What a participant says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Self-asserted (D5): trimmed, at most [`NAME_LIMIT`] characters.
    pub name: String,
}

impl Profile {
    /// A profile with `name`, trimmed and cut to [`NAME_LIMIT`] characters.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.trim().chars().take(NAME_LIMIT).collect(),
        }
    }
}

/// A profile as the track carries it: JSON, one frame a group.
#[derive(Serialize, Deserialize)]
struct ProfileFrame {
    name: String,
    /// The participant's key in the room, hex.
    key: String,
}

impl ProfileFrame {
    pub(crate) fn encode(profile: &Profile, key: &ParticipantKey) -> Vec<u8> {
        let frame = Self {
            name: profile.name.clone(),
            key: key.to_string(),
        };
        serde_json::to_vec(&frame).expect("a profile serializes")
    }

    /// The participant a profile frame describes; none if it isn't one.
    fn decode(bytes: &[u8]) -> Option<Participant> {
        let frame: Self = serde_json::from_slice(bytes).ok()?;
        let mut key = [0; 32];
        if frame.key.len() != 64 {
            return None;
        }
        for (byte, pair) in key.iter_mut().zip(frame.key.as_bytes().chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Participant {
            key: ParticipantKey(key),
            profile: Profile::new(&frame.name),
        })
    }
}

/// Writes `profile` as the latest group of `track`.
pub(crate) fn write_profile(
    track: &mut moq_e2ee::track::Producer,
    profile: &Profile,
    key: &ParticipantKey,
) -> Result<(), TrackError> {
    let mut group = track.append_group()?;
    group.write_frame(
        moq_net::Timestamp::now(),
        &ProfileFrame::encode(profile, key),
    )?;
    group.finish()?;
    Ok(())
}

/// Someone else in the room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub key: ParticipantKey,
    pub profile: Profile,
}

/// The other participants, in the order they joined, the same for everyone in the room.
pub type Roster = Vec<Participant>;

/// Follows the room's peers and their profiles into `roster` until the peers end or nobody
/// watches the roster any more.
pub(crate) async fn follow(mut peers: Peers, roster: watch::Sender<Roster>) {
    // Each peer's profiles as they come, from a task of its own, by its broadcast's path.
    let (profiles, mut updates) = mpsc::unbounded_channel::<(String, Participant)>();
    let mut readers: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    // Who is at which path.
    let mut present: Vec<(String, Participant)> = Vec::new();
    loop {
        tokio::select! {
            event = peers.next() => match event {
                Some(Ok(PeerEvent::Arrived(peer))) => {
                    let path = peer.path().to_owned();
                    let reader = tokio::spawn(read_profiles(peer, profiles.clone()));
                    if let Some(former) = readers.insert(path, reader) {
                        former.abort();
                    }
                }
                Some(Ok(PeerEvent::Gone(path))) => {
                    if let Some(reader) = readers.remove(&path) {
                        reader.abort();
                    }
                    present.retain(|(at, _)| *at != path);
                }
                // A broadcast that isn't a participant's: not someone in the room.
                Some(Err(error)) => {
                    tracing::debug!(%error, "a broadcast in the room that isn't a participant's");
                    continue;
                }
                None => break,
            },
            Some((path, participant)) = updates.recv() => {
                // A rejoin is a new broadcast with the same key: the newer one stands.
                present.retain(|(at, known)| *at == path || known.key != participant.key);
                match present.iter_mut().find(|(at, _)| *at == path) {
                    Some((_, known)) => *known = participant,
                    None => {
                        present.push((path, participant));
                        // By when each joined: the epoch ending a path is a UUIDv7, which sorts
                        // by time. Arrival here isn't that order for someone who joins late.
                        present.sort_by(|(left, _), (right, _)| epoch(left).cmp(epoch(right)));
                    }
                }
            }
            () = roster.closed() => break,
        }
        roster.send_if_modified(|shown| {
            let now: Roster = present
                .iter()
                .map(|(_, participant)| participant.clone())
                .collect();
            let changed = *shown != now;
            *shown = now;
            changed
        });
    }
    for reader in readers.into_values() {
        reader.abort();
    }
}

/// The publisher instance's epoch, which ends a broadcast's path.
fn epoch(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, epoch)| epoch)
}

/// `peer`'s profile, and each change to it, into `profiles`, until its track ends.
async fn read_profiles(peer: Peer, profiles: mpsc::UnboundedSender<(String, Participant)>) {
    let mut track = match peer.track(PROFILE).await {
        Ok(track) => track,
        Err(error) => {
            tracing::debug!(%error, path = peer.path(), "no profile");
            return;
        }
    };
    loop {
        let mut group = match track.recv_group().await {
            Ok(Some(group)) => group,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, path = peer.path(), "the profile track failed");
                return;
            }
        };
        let Ok(Some(frame)) = group.read_frame().await else {
            continue;
        };
        if let Some(participant) = ProfileFrame::decode(&frame.plaintext)
            && profiles
                .send((peer.path().to_owned(), participant))
                .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_reads_back_as_written() {
        let key = ParticipantKey([0xab; 32]);
        let profile = Profile::new("Alice");
        let read = ProfileFrame::decode(&ProfileFrame::encode(&profile, &key)).unwrap();
        assert_eq!(read, Participant { key, profile });
    }

    #[test]
    fn a_name_is_trimmed_and_cut() {
        assert_eq!(Profile::new("  Bob \n").name, "Bob");
        assert_eq!(
            Profile::new(&"x".repeat(100)).name.chars().count(),
            NAME_LIMIT
        );
        assert_eq!(
            Profile::new(&"ä".repeat(100)).name.chars().count(),
            NAME_LIMIT
        );
    }

    #[test]
    fn what_isnt_a_profile_is_none() {
        assert!(ProfileFrame::decode(b"not json").is_none());
        assert!(ProfileFrame::decode(br#"{"name": "x", "key": "zz"}"#).is_none());
    }
}
