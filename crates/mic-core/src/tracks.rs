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

use crate::invite::Invite;

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

    /// The generation of the broadcast at `path`, from its last segment, the epoch; `path` is
    /// relative to the room's prefix, as announcements under it name broadcasts.
    pub fn generation_of(&self, path: &str) -> Result<Generation, TrackError> {
        let not_a_participant = || TrackError::NotAParticipant(path.to_owned());
        let (_participant, epoch) = path.rsplit_once('/').ok_or_else(not_a_participant)?;
        let epoch = epoch.parse::<Epoch>().map_err(|_| not_a_participant())?;
        Ok(self.credential.generation(epoch))
    }
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// This participant's broadcast in the room: every track of it encrypted.
pub struct Publication {
    generation: Generation,
    broadcast: moq_net::broadcast::Producer,
    announced: bool,
}

impl Publication {
    pub(crate) fn new(generation: Generation, broadcast: moq_net::broadcast::Producer) -> Self {
        Self {
            generation,
            broadcast,
            announced: false,
        }
    }

    /// A track `semantic` of this broadcast, encrypted; the broadcast is announced once it has
    /// its first track, so a subscriber acting on the announcement finds it.
    pub fn track(&mut self, semantic: &str) -> Result<moq_e2ee::track::Producer, TrackError> {
        let name = self.generation.name(semantic)?;
        let track = self.broadcast.create_track(name.as_str(), None)?;
        let track = self.generation.produce(track)?;
        if !self.announced {
            self.broadcast.announce(Default::default())?;
            self.announced = true;
        }
        Ok(track)
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
        }
    }

    /// The next arrival or departure; none once the session's origin is gone. A broadcast that
    /// isn't shaped as a participant's is an error, and the next call goes on.
    pub async fn next(&mut self) -> Option<Result<PeerEvent, TrackError>> {
        loop {
            let update = self.announced.next().await?;
            let path = update.prefix.as_str().to_owned();
            if path == self.own {
                continue;
            }
            if !update.kind.is_active() {
                return Some(Ok(PeerEvent::Gone(path)));
            }
            return Some(self.arrived(path).await);
        }
    }

    async fn arrived(&self, path: String) -> Result<PeerEvent, TrackError> {
        let generation = self.crypto.generation_of(&path)?;
        let broadcast = self.consumer.request_broadcast(path.as_str()).await?;
        Ok(PeerEvent::Arrived(Peer::new(path, generation, broadcast)))
    }
}

/// Someone's broadcast in the room, found under its prefix.
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

    /// Its track `semantic`, decrypted as it's read: a frame that doesn't open fails the track.
    pub async fn track(&self, semantic: &str) -> Result<moq_e2ee::track::Consumer, TrackError> {
        let name = self.generation.name(semantic)?;
        let track = self.broadcast.track(name.as_str())?.subscribe(None).await?;
        Ok(self.generation.consume(track)?)
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
