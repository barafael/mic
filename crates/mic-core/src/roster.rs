//! Who is in the room (§1.5, §5.2): the broadcasts announced under the room's prefix whose signed
//! catalog stands (`crate::catalog`), which says whose each is, with the profile each publishes on
//! its `profile` track, the latest group its profile. A participant that leaves, or whose session
//! ends, is unannounced, and leaves the roster. A broadcast whose catalog doesn't stand is listed
//! apart, with why: someone in the room is publishing what they can't sign.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::catalog::{CATALOG, Catalog, Expected};
use crate::invite::RoomId;
use crate::sig::{self, SIG, SignedTrack, Verification};
use crate::tracks::{Peer, PeerEvent, Peers, RoomCrypto, TrackError};

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

impl std::str::FromStr for ParticipantKey {
    type Err = &'static str;

    /// 64 hex digits.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let not_a_key = "not a participant's key: 64 hex digits";
        // Hex digits only: `u8::from_str_radix` would also take a sign, as in "+f".
        if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(not_a_key);
        }
        let mut key = [0; 32];
        for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| not_a_key)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| not_a_key)?;
        }
        Ok(Self(key))
    }
}

/// As its hex digits, in the room's state and the harness.
impl Serialize for ParticipantKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ParticipantKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for ParticipantKey {
    /// The first 8 hex digits: enough to tell participants apart in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ParticipantKey({}…)", &self.to_string()[..8])
    }
}

/// What a participant says about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Self-asserted (D5): trimmed, at most [`NAME_LIMIT`] characters.
    pub name: String,
    /// Whether it sends no voice, by its own word: a tile shows it.
    #[serde(default)]
    pub muted: bool,
    /// Whether its camera is off, by its own word: its tile shows its name, not its last picture.
    #[serde(default)]
    pub camera_off: bool,
    /// Whether its hand is raised (§5.6).
    #[serde(default)]
    pub hand: bool,
    /// Whether it shares its screen: its `screen` track has pictures.
    #[serde(default)]
    pub sharing: bool,
}

impl Profile {
    /// A profile with `name`, trimmed and cut to [`NAME_LIMIT`] characters, not muted.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.trim().chars().take(NAME_LIMIT).collect(),
            muted: false,
            camera_off: false,
            hand: false,
            sharing: false,
        }
    }
}

/// A profile as the track carries it: JSON, one frame a group. Whose it is, the catalog says.
#[derive(Serialize, Deserialize)]
struct ProfileFrame {
    name: String,
    /// Left out by a client from before it existed: not muted.
    #[serde(default)]
    muted: bool,
    /// Likewise: the camera on.
    #[serde(default)]
    camera_off: bool,
    /// Likewise: no hand raised.
    #[serde(default)]
    hand: bool,
    /// Likewise: no screen shared.
    #[serde(default)]
    sharing: bool,
}

impl ProfileFrame {
    fn encode(profile: &Profile) -> Vec<u8> {
        let frame = Self {
            name: profile.name.clone(),
            muted: profile.muted,
            camera_off: profile.camera_off,
            hand: profile.hand,
            sharing: profile.sharing,
        };
        serde_json::to_vec(&frame).expect("a profile serializes")
    }

    /// The profile a frame carries; none if it isn't one.
    fn decode(bytes: &[u8]) -> Option<Profile> {
        let frame: Self = serde_json::from_slice(bytes).ok()?;
        Some(Profile {
            muted: frame.muted,
            camera_off: frame.camera_off,
            hand: frame.hand,
            sharing: frame.sharing,
            ..Profile::new(&frame.name)
        })
    }
}

/// Writes `profile` as the latest group of `track`.
pub(crate) fn write_profile(track: &mut SignedTrack, profile: &Profile) -> Result<(), TrackError> {
    let mut group = track.append_group()?;
    group.write_frame(moq_net::Timestamp::now(), &ProfileFrame::encode(profile))?;
    group.finish()?;
    Ok(())
}

/// Someone in the room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub key: ParticipantKey,
    pub profile: Profile,
    /// The video codecs it decodes, by name, as its catalog says.
    #[serde(default)]
    pub decoders: Vec<String>,
    /// Which codec each of its video tracks is sent in.
    #[serde(default)]
    pub renditions: Vec<crate::catalog::Rendition>,
}

/// Who is in the room, as `follow` keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    /// The other participants, in the order they joined, the same for everyone in the room.
    pub participants: Vec<Participant>,
    /// Broadcasts in the room whose catalog doesn't stand, by path.
    pub rejected: Vec<Rejected>,
}

/// A broadcast in the room whose catalog doesn't stand: someone with the room's secret publishing
/// what they can't sign, as when they pose as someone else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    /// Its path under the room's prefix.
    pub path: String,
    pub reason: String,
}

/// What a peer's readers find.
enum Found {
    /// Its catalog stands: whose the broadcast is, and what it decodes and sends.
    Verified(Box<Catalog>),
    Profile(Profile),
    Rejected(String),
}

/// What `follow` knows of a broadcast.
#[derive(Default)]
struct Known {
    key: Option<ParticipantKey>,
    profile: Option<Profile>,
    decoders: Vec<String>,
    renditions: Vec<crate::catalog::Rendition>,
}

/// Where the others' media plays, if anywhere: voices into a mixer, cameras onto a board, those
/// whose codec this participant decodes.
#[derive(Clone)]
pub(crate) struct Playing {
    pub mixer: Option<mic_media::audio::Mixer>,
    pub board: crate::video::Board,
    /// Where the others' screens show.
    pub screens: crate::video::Board,
    /// The video codecs this participant decodes, by name.
    pub decoders: Vec<String>,
    /// When each voice was last loud, as its player hears it.
    pub voices: crate::speaker::Voices,
    /// Whose cameras are shown, and so played: all, if none are named.
    pub watching: watch::Receiver<Option<Vec<ParticipantKey>>>,
    /// The active speaker, whose camera is raised above the others'.
    pub speaker: watch::Receiver<Option<ParticipantKey>>,
    /// Where what's said in chat and reactions goes.
    pub heard: mpsc::UnboundedSender<crate::chat::Heard>,
}

impl Playing {
    /// Whether `whose` camera is shown.
    fn shows(&self, whose: ParticipantKey) -> bool {
        self.watching
            .borrow()
            .as_ref()
            .is_none_or(|watching| watching.contains(&whose))
    }
}

/// Follows the room's peers, their catalogs and their profiles into `roster` until the peers end
/// or nobody watches the roster any more.
pub(crate) async fn follow(
    mut peers: Peers,
    crypto: RoomCrypto,
    room: RoomId,
    roster: watch::Sender<Roster>,
    playing: Playing,
) {
    // What each peer's readers find, from tasks of their own, by its broadcast's path.
    let (found, mut finds) = mpsc::unbounded_channel::<(String, Found)>();
    let mut readers: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut known: HashMap<String, Known> = HashMap::new();
    let mut rejected: Vec<Rejected> = Vec::new();
    loop {
        tokio::select! {
            event = peers.next() => match event {
                Some(Ok(PeerEvent::Arrived(peer))) => {
                    let path = peer.path().to_owned();
                    known.insert(path.clone(), Known::default());
                    rejected.retain(|gone| gone.path != path);
                    let reader = tokio::spawn(read_peer(
                        peer,
                        crypto.clone(),
                        room,
                        found.clone(),
                        playing.clone(),
                    ));
                    if let Some(former) = readers.insert(path, reader) {
                        former.abort();
                    }
                }
                Some(Ok(PeerEvent::Gone(path))) => {
                    if let Some(reader) = readers.remove(&path) {
                        reader.abort();
                    }
                    known.remove(&path);
                    rejected.retain(|gone| gone.path != path);
                }
                // A broadcast that isn't a participant's: not someone in the room.
                Some(Err(error)) => {
                    tracing::debug!(%error, "a broadcast in the room that isn't a participant's");
                    continue;
                }
                None => break,
            },
            Some((path, find)) = finds.recv() => {
                // Found by a reader before its peer went: the peer is gone all the same.
                let Some(entry) = known.get_mut(&path) else {
                    continue;
                };
                match find {
                    Found::Verified(catalog) => {
                        entry.key = Some(catalog.participant);
                        entry.decoders = catalog.decoders;
                        entry.renditions = catalog.renditions;
                    }
                    Found::Profile(profile) => entry.profile = Some(profile),
                    Found::Rejected(reason) => {
                        tracing::warn!(%path, %reason, "a broadcast in the room doesn't stand");
                        known.remove(&path);
                        if let Some(reader) = readers.remove(&path) {
                            reader.abort();
                        }
                        rejected.push(Rejected { path, reason });
                    }
                }
            }
            () = roster.closed() => break,
        }
        let now = Roster {
            participants: participants(&known),
            rejected: rejected.clone(),
        };
        roster.send_if_modified(|shown| {
            let changed = *shown != now;
            *shown = now;
            changed
        });
    }
    for reader in readers.into_values() {
        reader.abort();
    }
}

/// The participants among the broadcasts known, by when each joined: the epoch ending a path is a
/// UUIDv7, which sorts by time. Of two broadcasts of one key (a rejoin, before the first is
/// gone), the newer stands.
fn participants(known: &HashMap<String, Known>) -> Vec<Participant> {
    let mut standing: Vec<(&str, Participant)> = known
        .iter()
        .filter_map(|(path, known)| {
            Some((
                epoch(path),
                Participant {
                    key: known.key?,
                    profile: known.profile.clone()?,
                    decoders: known.decoders.clone(),
                    renditions: known.renditions.clone(),
                },
            ))
        })
        .collect();
    standing.sort_by_key(|(epoch, _)| *epoch);
    let mut participants: Vec<Participant> = Vec::new();
    for (_, participant) in standing.into_iter().rev() {
        if !participants
            .iter()
            .any(|newer| newer.key == participant.key)
        {
            participants.push(participant);
        }
    }
    participants.reverse();
    participants
}

/// The publisher instance's epoch, which ends a broadcast's path.
fn epoch(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, epoch)| epoch)
}

/// Stops a task when dropped.
struct Task(tokio::task::JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `peer`'s catalog, and each one after it, into `found`: nothing of the broadcast is believed
/// before its first catalog stands, and once one doesn't, the broadcast is rejected. Once it
/// stands, its checkpoints are verified with the key it names, and its profiles read; a frame the
/// checkpoints don't confirm rejects the broadcast too.
async fn read_peer(
    peer: Peer,
    crypto: RoomCrypto,
    room: RoomId,
    found: mpsc::UnboundedSender<(String, Found)>,
    mut playing: Playing,
) {
    let path = peer.path().to_owned();
    let send = |find: Found| found.send((path.clone(), find)).is_ok();
    let mut catalogs = match peer.track(CATALOG).await {
        Ok(track) => track,
        Err(error) => {
            send(Found::Rejected(format!("no catalog: {error}")));
            return;
        }
    };
    let segment_of = |key: &ParticipantKey| crypto.participant_segment(key.as_bytes()).ok();
    let mut last: Option<u64> = None;
    let mut _profiles: Option<Task> = None;
    // Its voice, played once a catalog names it, and its camera, while it's shown, until the
    // broadcast goes or is rejected.
    let mut _voice: Option<Task> = None;
    let mut _chat: Vec<Task> = Vec::new();
    let mut camera: Option<Camera> = None;
    // Its screen: subscribed to once its catalog names one, whether it's shared now or not, as
    // a track without pictures costs nothing.
    let mut _screen: Option<Task> = None;
    // Whose camera the catalog names, in a codec this participant decodes.
    let mut decodable: Option<ParticipantKey> = None;
    let mut verification: Option<Verification> = None;
    let mut verifying: Option<tokio::task::JoinHandle<sig::Forgery>> = None;
    // Once the room's task is gone, these are never asked again.
    let (mut watching_open, mut speaker_open) = (true, true);
    loop {
        let received = tokio::select! {
            received = catalogs.recv_group() => Some(received),
            forgery = async {
                match verifying.as_mut() {
                    Some(task) => task.await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                if let Some(forgery) = forgery {
                    send(Found::Rejected(forgery.to_string()));
                }
                return;
            }
            changed = playing.watching.changed(), if watching_open => {
                watching_open = changed.is_ok();
                playing.watching.mark_unchanged();
                None
            }
            changed = playing.speaker.changed(), if speaker_open => {
                speaker_open = changed.is_ok();
                playing.speaker.mark_unchanged();
                None
            }
        };
        if let Some(received) = received {
            let mut group = match received {
                Ok(Some(group)) => group,
                Ok(None) => return,
                Err(error) => {
                    send(Found::Rejected(format!(
                        "its catalog track failed: {error}"
                    )));
                    return;
                }
            };
            let Ok(Some(frame)) = group.read_frame().await else {
                continue;
            };
            let expected = Expected {
                room,
                kid: crypto.kid(),
                epoch: peer.epoch(),
                participant_segment: peer.participant_segment(),
                segment_of: &segment_of,
                last,
            };
            let catalog = match Catalog::verify(&frame.plaintext, &expected) {
                Ok(catalog) => catalog,
                Err(rejection) => {
                    send(Found::Rejected(rejection.to_string()));
                    return;
                }
            };
            last = Some(catalog.seq);
            if !send(Found::Verified(Box::new(catalog.clone()))) {
                return;
            }
            if verifying.is_none() {
                let checkpoints = match peer.track(SIG).await {
                    Ok(track) => track,
                    Err(error) => {
                        send(Found::Rejected(format!("no checkpoints: {error}")));
                        return;
                    }
                };
                let checked = Verification::default();
                verification = Some(checked.clone());
                verifying = Some(tokio::spawn(sig::verify(
                    checkpoints,
                    checked.clone(),
                    catalog.participant,
                    room,
                    crypto.kid(),
                    peer.epoch().to_owned(),
                )));
                _profiles = Some(Task(tokio::spawn(read_profiles(
                    peer.clone(),
                    checked,
                    found.clone(),
                ))));
            }
            // What the catalog names now, started once: a track added later starts with the
            // catalog that names it.
            if _voice.is_none()
                && let Some(checked) = &verification
                && let Some(mixer) = &playing.mixer
                && catalog
                    .tracks
                    .iter()
                    .any(|track| track == crate::audio::AUDIO)
            {
                _voice = play_voice(
                    &peer,
                    checked,
                    mixer.clone(),
                    catalog.participant,
                    playing.voices.clone(),
                )
                .await;
            }
            if _chat.is_empty()
                && let Some(checked) = &verification
            {
                for track in [crate::chat::CHAT, crate::chat::REACTIONS] {
                    if catalog.tracks.iter().any(|named| named == track) {
                        _chat.push(Task(tokio::spawn(crate::chat::read(
                            peer.clone(),
                            track,
                            checked.clone(),
                            catalog.participant,
                            playing.heard.clone(),
                        ))));
                    }
                }
            }
            if _screen.is_none()
                && let Some(checked) = &verification
                && catalog.renditions.iter().any(|rendition| {
                    rendition.track == crate::video::SCREEN
                        && playing.decoders.contains(&rendition.codec)
                })
            {
                match peer.verified_track(crate::video::SCREEN, checked).await {
                    Ok(track) => {
                        _screen = Some(Task(tokio::spawn(crate::video::play(
                            track,
                            catalog.participant,
                            playing.screens.clone(),
                        ))));
                    }
                    Err(error) => tracing::debug!(%error, path = peer.path(), "no screen"),
                }
            }
            if catalog.renditions.iter().any(|rendition| {
                rendition.track == crate::video::CAMERA
                    && playing.decoders.contains(&rendition.codec)
            }) {
                decodable = Some(catalog.participant);
            }
        }
        // Its camera, played while it's shown, and raised while it's the active speaker's.
        let (Some(checked), Some(whose)) = (&verification, decodable) else {
            continue;
        };
        let shown = playing.shows(whose);
        if shown && camera.is_none() {
            camera = play_camera(&peer, checked, whose, &playing.board).await;
        } else if !shown && let Some(unwatched) = camera.take() {
            unwatched.stop(checked);
        }
        if let Some(camera) = camera.as_mut() {
            camera.raise(*playing.speaker.borrow() == Some(whose));
        }
    }
}

/// A camera being played, its subscription raised while its sender is the active speaker.
/// Dropped, it stops.
struct Camera {
    playing: tokio::task::JoinHandle<()>,
    preferences: crate::tracks::Preferences,
    raised: bool,
}

impl Camera {
    /// Stops playing it, as when it's no longer shown: what was read of its group and isn't
    /// confirmed yet never will be, so it's let go (`Verification::abandon`).
    fn stop(self, verification: &Verification) {
        self.playing.abort();
        verification.abandon(crate::video::CAMERA);
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.playing.abort();
    }
}

impl Camera {
    fn raise(&mut self, raised: bool) {
        if self.raised != raised {
            self.raised = raised;
            self.preferences.raise(raised);
        }
    }
}

/// `peer`'s camera onto `board`, its frames confirmed as any others are.
async fn play_camera(
    peer: &Peer,
    verification: &Verification,
    whose: ParticipantKey,
    board: &crate::video::Board,
) -> Option<Camera> {
    match peer
        .verified_track_with_preferences(crate::video::CAMERA, verification)
        .await
    {
        Ok((track, preferences)) => Some(Camera {
            playing: tokio::spawn(crate::video::play(track, whose, board.clone())),
            preferences,
            raised: false,
        }),
        Err(error) => {
            tracing::debug!(%error, path = peer.path(), "no camera");
            None
        }
    }
}

/// `peer`'s voice, `whose` it is, into a stream of `mixer`'s, its frames confirmed as any others
/// are, and how loud it is into `voices`.
async fn play_voice(
    peer: &Peer,
    verification: &Verification,
    mixer: mic_media::audio::Mixer,
    whose: ParticipantKey,
    voices: crate::speaker::Voices,
) -> Option<Task> {
    match peer.verified_track(crate::audio::AUDIO, verification).await {
        Ok(track) => Some(Task(tokio::spawn(crate::audio::play(
            track, mixer, whose, voices,
        )))),
        Err(error) => {
            tracing::debug!(%error, path = peer.path(), "no voice");
            None
        }
    }
}

/// `peer`'s profile, and each change to it, into `found`, until its track ends: shown at once,
/// and confirmed by its checkpoints within the grace (`verification`).
async fn read_profiles(
    peer: Peer,
    verification: Verification,
    found: mpsc::UnboundedSender<(String, Found)>,
) {
    let mut track = match peer.verified_track(PROFILE, &verification).await {
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
        if let Some(profile) = ProfileFrame::decode(&frame.plaintext)
            && found
                .send((peer.path().to_owned(), Found::Profile(profile)))
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
        let profile = Profile::new("Alice");
        assert_eq!(
            ProfileFrame::decode(&ProfileFrame::encode(&profile)),
            Some(profile)
        );
    }

    #[test]
    fn of_two_broadcasts_of_one_key_the_newer_stands() {
        let known = |key: u8, name: &str| Known {
            key: Some(ParticipantKey([key; 32])),
            profile: Some(Profile::new(name)),
            decoders: Vec::new(),
            renditions: Vec::new(),
        };
        let known: HashMap<String, Known> = [
            (
                "a/01a17a43-0000-7000-8000-000000000003".to_owned(),
                known(1, "Alice again"),
            ),
            (
                "b/01a17a43-0000-7000-8000-000000000002".to_owned(),
                known(2, "Bob"),
            ),
            (
                "a/01a17a43-0000-7000-8000-000000000001".to_owned(),
                known(1, "Alice"),
            ),
        ]
        .into_iter()
        .collect();
        let names: Vec<String> = participants(&known)
            .into_iter()
            .map(|participant| participant.profile.name)
            .collect();
        assert_eq!(names, ["Bob", "Alice again"]);
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
    fn a_key_reads_back_from_its_hex() {
        let key = ParticipantKey([0x5c; 32]);
        assert_eq!(key.to_string().parse(), Ok(key));
        assert!("5c".parse::<ParticipantKey>().is_err());
        assert!(
            format!("+f{}", &key.to_string()[2..])
                .parse::<ParticipantKey>()
                .is_err()
        );
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<ParticipantKey>(&json).unwrap(), key);
    }

    #[test]
    fn muted_reads_back_and_defaults_to_not() {
        let muted = Profile {
            muted: true,
            ..Profile::new("Alice")
        };
        assert_eq!(
            ProfileFrame::decode(&ProfileFrame::encode(&muted)),
            Some(muted)
        );
        assert_eq!(
            ProfileFrame::decode(br#"{"name": "Bob"}"#),
            Some(Profile::new("Bob"))
        );
    }

    #[test]
    fn what_isnt_a_profile_is_none() {
        assert!(ProfileFrame::decode(b"not json").is_none());
        assert!(ProfileFrame::decode(br#"{"nickname": "x"}"#).is_none());
    }
}
