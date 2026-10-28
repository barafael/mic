//! Every track mic publishes or subscribes goes through here, and is encrypted with `moq-e2ee`
//! under the room's secret (D17): media, catalogs, profiles, chat, signatures alike. There is no
//! way in this crate to publish or subscribe a track in the clear.
//!
//! A room's broadcasts sit under one opaque prefix, which the relay sees, so it learns which
//! broadcasts form a room, but not who is in it (§5.2):
//!
//! ```text
//! <room prefix>/<participant>/<epoch>
//! ```
//!
//! - `<room prefix>`: HKDF-SHA256 of the room secret, with "mic/v1/room-prefix" and the room id
//!   as info, 16 bytes in base64url.
//! - `<participant>`: moq-e2ee's opaque path of the participant's public key in the room.
//! - `<epoch>`: the publisher instance's (a UUIDv7): a rejoin is a new one.
//!
//! Track names are moq-e2ee's opaque names of their semantic names, scoped to the epoch.

use hkdf::Hkdf;
use moq_e2ee::{Credential, Epoch, Generation};
use moq_net::{Path, PathOwned};
use sha2::Sha256;

use crate::catalog::{CATALOG, Catalog};
use crate::invite::{Invite, RoomId};
use crate::keys::RoomKey;
use crate::roster::ParticipantKey;
use crate::sig::{self, Checkpoint, Ledger, SIG, SignedTrack, Verification, VerifiedTrack};

/// The HKDF info's prefix for a room's broadcast prefix.
const ROOM_PREFIX: &[u8] = b"mic/v1/room-prefix";
/// The context moq-e2ee binds every derivation to: versioned, and the room's.
const CONTEXT: &[u8] = b"mic/v1/room/";
/// The credential's key id: one static secret per room in v1. MLS (§5.3.3) rotates it.
const KID: u64 = 0;

#[derive(Debug, derive_more::Display, derive_more::Error)]
pub enum TrackError {
    /// moq-e2ee refused: a name or context out of its bounds, or a frame that didn't open (a
    /// wrong secret, or a forgery).
    #[display("moq-e2ee: {_0}")]
    E2ee(moq_e2ee::Error),
    #[display("moq: {_0}")]
    Net(moq_net::Error),
    /// A broadcast under the room's prefix that isn't shaped as a participant's.
    #[display("{_0:?} isn't a participant's broadcast")]
    NotAParticipant(#[error(not(source))] String),
    /// A codec refused: its settings, or a packet.
    #[display("a codec: {_0}")]
    Codec(#[error(not(source))] String),
}

impl From<moq_e2ee::Error> for TrackError {
    fn from(error: moq_e2ee::Error) -> Self {
        Self::E2ee(error)
    }
}

impl From<moq_net::Error> for TrackError {
    fn from(error: moq_net::Error) -> Self {
        Self::Net(error)
    }
}

/// A room's encryption: its credential, and the prefix its broadcasts sit under.
#[derive(Clone, Debug)]
pub struct RoomCrypto {
    credential: Credential,
    prefix: PathOwned,
}

impl RoomCrypto {
    pub fn new(invite: &Invite) -> Result<Self, TrackError> {
        let mut context = CONTEXT.to_vec();
        context.extend_from_slice(invite.room.as_bytes());
        let credential = Credential::new(moq_e2ee::credential::Config {
            context: context.into(),
            kid: KID,
            secret: *invite.secret.as_bytes(),
        })?;
        let mut info = ROOM_PREFIX.to_vec();
        info.extend_from_slice(invite.room.as_bytes());
        let mut prefix = [0; 16];
        Hkdf::<Sha256>::new(None, invite.secret.as_bytes())
            .expand(&info, &mut prefix)
            .expect("16 bytes is a valid length for HKDF-SHA256");
        use base64::Engine;
        let prefix = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(prefix);
        Ok(Self {
            credential,
            prefix: Path::new(&prefix).into_owned(),
        })
    }

    /// The prefix every broadcast of the room sits under.
    pub fn prefix(&self) -> &Path<'_> {
        &self.prefix
    }

    /// A new publisher instance for `participant` (its public key in the room): its broadcast's
    /// path under the room's prefix, `<participant>/<epoch>`, and the generation its tracks are
    /// encrypted under.
    pub fn publisher(&self, participant: &[u8; 32]) -> Result<(PathOwned, Generation), TrackError> {
        let generation = self.credential.generation(Epoch::mint());
        let path = self
            .credential
            .path(&hex(participant))?
            .join(generation.epoch().as_str());
        Ok((path, generation))
    }

    /// The participant segment of `participant`'s broadcasts in the room: what a catalog's key
    /// must derive for the catalog to stand at a path (`crate::catalog`).
    pub fn participant_segment(&self, participant: &[u8; 32]) -> Result<String, TrackError> {
        Ok(self.credential.path(&hex(participant))?.as_str().to_owned())
    }

    /// The room credential's key id.
    pub fn kid(&self) -> u64 {
        KID
    }

    /// The generation of the broadcast at `path`, from its last segment, the epoch; `path` is
    /// relative to the room's prefix, as announcements under it name broadcasts.
    pub fn generation_of(&self, path: &str) -> Result<Generation, TrackError> {
        let not_a_participant = || TrackError::NotAParticipant(path.to_owned());
        let (_participant, epoch) = path.rsplit_once('/').ok_or_else(not_a_participant)?;
        let epoch = epoch.parse::<Epoch>().map_err(|_| not_a_participant())?;
        Ok(self.credential.generation(epoch))
    }
}

/// How a track is served (§1.6, §5.7): its priority, higher first (moq-lite), and how old a
/// group may get before it's skipped, at the publisher and in each subscription alike. The voice
/// comes before everything; the checkpoints next, since a frame waits on them to be confirmed;
/// the camera after them.
/// The rest take moq-net's defaults. The active speaker's camera is raised above the others'
/// ([`RAISED`]), still below the checkpoints.
fn policy(semantic: &str) -> Option<(u8, std::time::Duration)> {
    use std::time::Duration;
    match semantic {
        crate::audio::AUDIO => Some((100, Duration::from_millis(500))),
        SIG => Some((90, Duration::from_secs(1))),
        crate::video::CAMERA => Some((50, Duration::from_secs(2))),
        // Below the media; a message late is still worth having, a reaction isn't.
        crate::chat::CHAT => Some((30, Duration::from_secs(30))),
        crate::chat::REACTIONS => Some((30, Duration::from_secs(1))),
        _ => None,
    }
}

/// The priority of the active speaker's camera, in each subscriber's subscription to it (§1.6).
pub(crate) const RAISED: u8 = 60;

/// How `semantic` is subscribed to: by its [`policy`], its priority `raised` to [`RAISED`] if
/// asked.
fn subscription(semantic: &str, raised: bool) -> Option<moq_net::track::Subscription> {
    policy(semantic).map(|(priority, max_age)| {
        moq_net::track::Subscription::default()
            .with_priority(if raised {
                RAISED.max(priority)
            } else {
                priority
            })
            .with_max_age(max_age)
    })
}

/// A subscription's preferences, changed as it runs: a camera raised as its sender speaks.
pub struct Preferences {
    semantic: String,
    control: moq_net::track::Control,
}

impl Preferences {
    /// Raises the track to [`RAISED`], or back to its policy's priority.
    pub fn raise(&self, raised: bool) {
        if let Some(subscription) = subscription(&self.semantic, raised)
            && let Err(error) = self.control.update(subscription)
        {
            tracing::debug!(%error, semantic = self.semantic, "a track's subscription ended");
        }
    }
}

/// Track `semantic` of `broadcast`, encrypted under `generation`, served by its [`policy`].
fn produce(
    generation: &Generation,
    broadcast: &moq_net::broadcast::Producer,
    semantic: &str,
) -> Result<moq_e2ee::track::Producer, TrackError> {
    let name = generation.name(semantic)?;
    let info = policy(semantic).map(|(priority, max_age)| {
        moq_net::track::Info::default()
            .with_priority(priority)
            .with_max_age(max_age)
    });
    let track = broadcast.create_track(name.as_str(), info)?;
    Ok(generation.produce(track)?)
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// This participant's broadcast in the room: every track of it encrypted, its first the signed
/// catalog (`crate::catalog`), signed again with every track added, its second the checkpoints
/// (`crate::sig`) that sign every other track's frames.
pub struct Publication {
    generation: Generation,
    broadcast: moq_net::broadcast::Producer,
    key: RoomKey,
    catalog: Catalog,
    /// Until it's finished, at close.
    catalog_track: Option<moq_e2ee::track::Producer>,
    ledger: Ledger,
    /// Signs the checkpoints for as long as the broadcast is up, and what stops it, its last
    /// checkpoint written.
    signing: Task,
    stop_signing: Option<tokio::sync::oneshot::Sender<()>>,
}

/// A task of the publication's, stopped with it.
struct Task(tokio::task::JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Publication {
    /// The broadcast with its catalog and checkpoints, announced: whoever finds it finds who
    /// publishes it. Starts signing on the runtime it's called on.
    pub(crate) fn new(
        generation: Generation,
        broadcast: moq_net::broadcast::Producer,
        key: RoomKey,
        room: RoomId,
        decoders: &[mic_media::video::Codec],
    ) -> Result<Self, TrackError> {
        let epoch = generation.epoch().as_str().to_owned();
        let catalog = Catalog {
            room,
            participant: ParticipantKey::from_bytes(key.public().to_bytes()),
            kid: KID,
            epoch: epoch.clone(),
            seq: 0,
            tracks: vec![CATALOG.to_owned(), SIG.to_owned()],
            decoders: decoders
                .iter()
                .map(|codec| codec.name().to_owned())
                .collect(),
            renditions: Vec::new(),
            app: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let catalog_track = produce(&generation, &broadcast, CATALOG)?;
        let sig_track = produce(&generation, &broadcast, SIG)?;
        let ledger = Ledger::default();
        let (stop_signing, stop) = tokio::sync::oneshot::channel();
        let signing = Task(tokio::spawn(sig::sign(
            ledger.clone(),
            sig_track,
            key.clone(),
            Checkpoint {
                room,
                kid: KID,
                epoch,
                seq: 0,
                entries: Vec::new(),
            },
            stop,
        )));
        let mut publication = Self {
            generation,
            broadcast,
            key,
            catalog,
            catalog_track: Some(catalog_track),
            ledger,
            signing,
            stop_signing: Some(stop_signing),
        };
        publication.write_catalog()?;
        publication.broadcast.announce(Default::default())?;
        Ok(publication)
    }

    /// A track `semantic` of this broadcast, encrypted, its frames covered by the checkpoints,
    /// and the catalog that names it signed.
    pub fn track(&mut self, semantic: &str) -> Result<SignedTrack, TrackError> {
        let track = produce(&self.generation, &self.broadcast, semantic)?;
        self.catalog.tracks.push(semantic.to_owned());
        self.catalog.seq += 1;
        self.write_catalog()?;
        Ok(SignedTrack::new(semantic, track, self.ledger.clone()))
    }

    /// A video track `semantic` of this broadcast, sent in `codec`, as [`Self::track`] is, its
    /// codec named in the catalog's renditions.
    pub fn video_track(
        &mut self,
        semantic: &str,
        codec: mic_media::video::Codec,
    ) -> Result<SignedTrack, TrackError> {
        self.catalog.renditions.push(crate::catalog::Rendition {
            track: semantic.to_owned(),
            codec: codec.name().to_owned(),
        });
        self.track(semantic)
    }

    /// The catalog, signed, as the track's latest group.
    fn write_catalog(&mut self) -> Result<(), TrackError> {
        let Some(catalog_track) = self.catalog_track.as_mut() else {
            return Ok(());
        };
        let mut group = catalog_track.append_group()?;
        group.write_frame(moq_net::Timestamp::now(), &self.catalog.sign(&self.key))?;
        group.finish()?;
        Ok(())
    }

    /// Ends the broadcast: its catalog finished, the checkpoints too once they've signed what's
    /// left (for up to a second), then it's unannounced, and its tracks end.
    pub async fn close(&mut self) {
        if let Some(catalog_track) = self.catalog_track.take()
            && let Err(error) = catalog_track.finish()
        {
            tracing::debug!(%error, "finishing the catalog");
        }
        if let Some(stop) = self.stop_signing.take() {
            let _stopped: Result<(), ()> = stop.send(());
            let signed =
                tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.signing.0);
            if signed.await.is_err() {
                tracing::debug!("the last checkpoint wasn't signed within a second");
            }
        }
        self.broadcast.close();
    }
}

/// What happened to a participant's broadcast in the room.
pub enum PeerEvent {
    /// It's there: its tracks can be read.
    Arrived(Peer),
    /// The broadcast at this path is gone.
    Gone(String),
}

/// The broadcasts that arrive and go under the room's prefix, but this participant's own.
pub struct Peers {
    crypto: RoomCrypto,
    own: String,
    consumer: moq_net::origin::Consumer,
    announced: moq_net::announce::Consumer,
    /// An arrival taken from the announcements whose broadcast isn't had yet: kept across a
    /// cancelled [`Peers::next`], so a `select!` that drops it loses no one.
    pending: Option<String>,
}

impl Peers {
    pub(crate) fn new(
        crypto: RoomCrypto,
        own: String,
        consumer: moq_net::origin::Consumer,
    ) -> Self {
        let announced = consumer.announced();
        Self {
            crypto,
            own,
            consumer,
            announced,
            pending: None,
        }
    }

    /// The next arrival or departure; none once the session's origin is gone. A broadcast that
    /// isn't shaped as a participant's is an error, and the next call goes on. Cancel-safe: an
    /// arrival whose broadcast was still being asked for is asked for again by the next call.
    pub async fn next(&mut self) -> Option<Result<PeerEvent, TrackError>> {
        loop {
            if let Some(path) = self.pending.clone() {
                let arrived = self.arrived(path).await;
                self.pending = None;
                return Some(arrived);
            }
            let update = self.announced.next().await?;
            let path = update.prefix.as_str().to_owned();
            if path == self.own {
                continue;
            }
            if !update.kind.is_active() {
                return Some(Ok(PeerEvent::Gone(path)));
            }
            self.pending = Some(path);
        }
    }

    async fn arrived(&self, path: String) -> Result<PeerEvent, TrackError> {
        let generation = self.crypto.generation_of(&path)?;
        let broadcast = self.consumer.request_broadcast(path.as_str()).await?;
        Ok(PeerEvent::Arrived(Peer::new(path, generation, broadcast)))
    }
}

/// Someone's broadcast in the room, found under its prefix.
#[derive(Clone)]
pub struct Peer {
    path: String,
    generation: Generation,
    broadcast: moq_net::broadcast::Consumer,
}

impl Peer {
    pub(crate) fn new(
        path: String,
        generation: Generation,
        broadcast: moq_net::broadcast::Consumer,
    ) -> Self {
        Self {
            path,
            generation,
            broadcast,
        }
    }

    /// Its path under the room's prefix: `<participant>/<epoch>`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Its path's participant segment.
    pub fn participant_segment(&self) -> &str {
        self.path
            .rsplit_once('/')
            .map_or("", |(participant, _)| participant)
    }

    /// Its publisher instance's epoch, which ends its path.
    pub fn epoch(&self) -> &str {
        self.generation.epoch().as_str()
    }

    /// Its track `semantic`, decrypted as it's read, its frames recorded for its checkpoints to
    /// confirm (`crate::sig`).
    pub async fn verified_track(
        &self,
        semantic: &str,
        verification: &Verification,
    ) -> Result<VerifiedTrack, TrackError> {
        Ok(VerifiedTrack::new(
            semantic,
            self.track(semantic).await?,
            verification.clone(),
        ))
    }

    /// As [`Self::verified_track`], with what changes its subscription as it runs.
    pub async fn verified_track_with_preferences(
        &self,
        semantic: &str,
        verification: &Verification,
    ) -> Result<(VerifiedTrack, Preferences), TrackError> {
        let (track, preferences) = self.subscribe(semantic).await?;
        Ok((
            VerifiedTrack::new(semantic, track, verification.clone()),
            preferences,
        ))
    }

    /// Its track `semantic`, decrypted as it's read: a frame that doesn't open fails the track.
    pub async fn track(&self, semantic: &str) -> Result<moq_e2ee::track::Consumer, TrackError> {
        Ok(self.subscribe(semantic).await?.0)
    }

    async fn subscribe(
        &self,
        semantic: &str,
    ) -> Result<(moq_e2ee::track::Consumer, Preferences), TrackError> {
        let name = self.generation.name(semantic)?;
        let track = self
            .broadcast
            .track(name.as_str())?
            .subscribe(subscription(semantic, false))
            .await?;
        let preferences = Preferences {
            semantic: semantic.to_owned(),
            control: track.control(),
        };
        Ok((self.generation.consume(track)?, preferences))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invite::{RoomId, RoomSecret};

    fn invite(secret: u8) -> Invite {
        Invite {
            room: RoomId::from_bytes([1; 16]),
            secret: RoomSecret::from_bytes([secret; 32]),
            name: None,
        }
    }

    #[test]
    fn a_rooms_prefix_comes_from_its_secret() {
        let one = RoomCrypto::new(&invite(2)).unwrap();
        assert_eq!(one.prefix(), RoomCrypto::new(&invite(2)).unwrap().prefix());
        assert_ne!(one.prefix(), RoomCrypto::new(&invite(3)).unwrap().prefix());
    }

    #[test]
    fn a_publishers_path_names_neither_room_nor_participant() {
        let crypto = RoomCrypto::new(&invite(2)).unwrap();
        let participant = [0xab; 32];
        let (path, generation) = crypto.publisher(&participant).unwrap();
        let path = path.as_str();
        assert!(!path.contains(&hex(&participant)));
        assert!(!path.contains(&RoomId::from_bytes([1; 16]).to_string()));
        assert!(path.ends_with(generation.epoch().as_str()));
        assert_eq!(
            crypto.generation_of(path).unwrap().epoch(),
            generation.epoch()
        );
    }

    #[test]
    fn a_rejoin_is_a_new_instance() {
        let crypto = RoomCrypto::new(&invite(2)).unwrap();
        let (first, _) = crypto.publisher(&[1; 32]).unwrap();
        let (again, _) = crypto.publisher(&[1; 32]).unwrap();
        assert_ne!(first, again);
        let (participant, _) = first.as_str().rsplit_once('/').unwrap();
        assert!(again.as_str().starts_with(participant));
    }
}
