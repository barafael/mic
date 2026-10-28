//! The proof that this client holds the room's invite and the path it publishes at, for a relay
//! that lets only members in (`mic-admit`, §5.11).
//!
//! Two keys sign it, each derived from what only they hold, and neither the key anyone is known
//! by in the room: the relay is shown the proof, and mustn't learn from it who signs a catalog.
//!
//! - the room's: `HKDF(room secret, "mic/v1/room-admission" || room id)`;
//! - the participant's: `HKDF(its room key's seed, "mic/v1/participant-admission" || room id)`.

use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use mic_admit::Proof;
use sha2::Sha256;
use zeroize::Zeroize;

use crate::invite::Invite;
use crate::keys::RoomKey;
use crate::tracks::{RoomCrypto, TrackError};

const ROOM: &[u8] = b"mic/v1/room-admission";
const PARTICIPANT: &[u8] = b"mic/v1/participant-admission";

/// `key`'s proof in the room of `invite`.
pub fn proof(invite: &Invite, crypto: &RoomCrypto, key: &RoomKey) -> Result<Proof, TrackError> {
    let room = derive(invite.secret.as_bytes(), ROOM, invite);
    let mut seed = key.signing().to_bytes();
    let participant = derive(&seed, PARTICIPANT, invite);
    seed.zeroize();
    let segment = crypto.participant_segment(&key.public().to_bytes())?;
    Proof::sign(crypto.prefix().as_str(), &segment, &room, &participant)
        .map_err(|error| TrackError::Admission(error.to_string()))
}

fn derive(ikm: &[u8; 32], purpose: &[u8], invite: &Invite) -> SigningKey {
    let mut info = purpose.to_vec();
    info.extend_from_slice(invite.room.as_bytes());
    let mut seed = [0; 32];
    Hkdf::<Sha256>::new(None, ikm)
        .expand(&info, &mut seed)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    let key = SigningKey::from_bytes(&seed);
    seed.zeroize();
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::DeviceSecret;

    fn proof_of(device: u8, invite: &Invite) -> Proof {
        let key = DeviceSecret::from_bytes([device; 32]).room_key(&invite.room);
        proof(invite, &RoomCrypto::new(invite).unwrap(), &key).unwrap()
    }

    #[test]
    fn a_participants_proof_is_stable_across_joins() {
        let invite = Invite::new_room();
        assert_eq!(proof_of(1, &invite), proof_of(1, &invite));
    }

    #[test]
    fn members_share_the_rooms_key_and_not_the_participants() {
        let invite = Invite::new_room();
        let (alice, bob) = (proof_of(1, &invite), proof_of(2, &invite));
        assert_eq!(alice.room(), bob.room());
        assert_eq!(alice.prefix(), bob.prefix());
        assert_ne!(alice.participant(), bob.participant());
        assert_ne!(alice.segment(), bob.segment());
    }

    #[test]
    fn another_rooms_proof_has_another_key_and_prefix() {
        let (one, other) = (Invite::new_room(), Invite::new_room());
        let (a, b) = (proof_of(1, &one), proof_of(1, &other));
        assert_ne!(a.room(), b.room());
        assert_ne!(a.prefix(), b.prefix());
    }

    #[test]
    fn the_relay_learns_no_key_anyone_is_known_by() {
        let invite = Invite::new_room();
        let key = DeviceSecret::from_bytes([1; 32]).room_key(&invite.room);
        let proof = proof_of(1, &invite);
        assert_ne!(proof.participant(), &key.public().to_bytes());
        assert_ne!(proof.room(), invite.secret.as_bytes());
    }
}
