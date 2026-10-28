//! The proof a client presents to be let into a room, in the query of the relay's URL:
//! `?admit=<base64url>`.
//!
//! It names the room's prefix and the participant's path, and carries two public keys, each
//! signing the same bytes:
//!
//! - the *room's* admission key, derived from the room's secret: only a holder of the invite can
//!   sign with it;
//! - the *participant's* admission key, derived from its key in the room, but not its identity:
//!   the relay learns nothing it could link to the participant's catalog or signatures.
//!
//! ```text
//! "mic/v1/admit\0" prefix_len:u8 prefix segment_len:u8 segment room:32 participant:32
//! room_signature:64 participant_signature:64
//! ```
//!
//! There's no time in it: a connection redials with the URL it has, for as long as the call
//! lasts, and a client's clock is not to be believed. A proof leaked to whoever reads the URL
//! grants what the invite grants, which they'd need the invite for.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use derive_more::{Display, Error};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// The query parameter the proof is in.
pub const PARAM: &str = "admit";
/// What every signature starts with: versioned, so no other signature of the room's keys is one.
const DOMAIN: &[u8] = b"mic/v1/admit\0";
/// The longest a prefix or path may be, in bytes.
const LIMIT: usize = 128;

#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum ProofError {
    #[display("not base64url")]
    Encoding,
    #[display("not a proof of this version")]
    Malformed,
    #[display("a path with characters a path doesn't have")]
    Path,
    #[display("a signature that isn't the key's")]
    Signature,
}

/// A client's proof that it holds a room's invite, and the path it publishes at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    prefix: String,
    segment: String,
    room: [u8; 32],
    participant: [u8; 32],
    room_signature: [u8; 64],
    participant_signature: [u8; 64],
}

impl Proof {
    /// `participant`'s proof in the room at `prefix`, publishing under `segment`, signed by the
    /// room's key and its own.
    pub fn sign(
        prefix: &str,
        segment: &str,
        room: &SigningKey,
        participant: &SigningKey,
    ) -> Result<Self, ProofError> {
        let (prefix, segment) = (prefix.trim_matches('/'), segment.trim_matches('/'));
        check_path(prefix)?;
        check_path(segment)?;
        let room_key = room.verifying_key().to_bytes();
        let participant_key = participant.verifying_key().to_bytes();
        let message = message(prefix, segment, &room_key, &participant_key);
        Ok(Self {
            prefix: prefix.to_owned(),
            segment: segment.to_owned(),
            room: room_key,
            participant: participant_key,
            room_signature: room.sign(&message).to_bytes(),
            participant_signature: participant.sign(&message).to_bytes(),
        })
    }

    /// The proof in `text`, its signatures checked.
    pub fn decode(text: &str) -> Result<Self, ProofError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| ProofError::Encoding)?;
        let mut reader = Reader(&bytes);
        reader
            .take(DOMAIN.len())
            .filter(|d| *d == DOMAIN)
            .ok_or(ProofError::Malformed)?;
        let prefix = reader.string()?;
        let segment = reader.string()?;
        let room = reader.array::<32>()?;
        let participant = reader.array::<32>()?;
        let room_signature = reader.array::<64>()?;
        let participant_signature = reader.array::<64>()?;
        if !reader.0.is_empty() {
            return Err(ProofError::Malformed);
        }
        check_path(&prefix)?;
        check_path(&segment)?;
        let message = message(&prefix, &segment, &room, &participant);
        for (key, signature) in [
            (&room, &room_signature),
            (&participant, &participant_signature),
        ] {
            let key = VerifyingKey::from_bytes(key).map_err(|_| ProofError::Signature)?;
            key.verify_strict(&message, &Signature::from_bytes(signature))
                .map_err(|_| ProofError::Signature)?;
        }
        Ok(Self {
            prefix,
            segment,
            room,
            participant,
            room_signature,
            participant_signature,
        })
    }

    /// For a URL's query.
    pub fn encode(&self) -> String {
        let mut bytes = message(&self.prefix, &self.segment, &self.room, &self.participant);
        bytes.extend_from_slice(&self.room_signature);
        bytes.extend_from_slice(&self.participant_signature);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// The room's prefix on the relay, without slashes.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The path under the prefix the participant publishes at.
    pub fn segment(&self) -> &str {
        &self.segment
    }

    /// The room's admission key.
    pub fn room(&self) -> &[u8; 32] {
        &self.room
    }

    /// The participant's admission key.
    pub fn participant(&self) -> &[u8; 32] {
        &self.participant
    }
}

/// A path is what moq-e2ee and the prefix make: base64url segments. Anything else would be a
/// pattern or a traversal in a grant.
fn check_path(path: &str) -> Result<(), ProofError> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/');
    let segments_ok = path.split('/').all(|segment| !segment.is_empty());
    if path.is_empty() || path.len() > LIMIT || !path.chars().all(allowed) || !segments_ok {
        return Err(ProofError::Path);
    }
    Ok(())
}

fn message(prefix: &str, segment: &str, room: &[u8; 32], participant: &[u8; 32]) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    for text in [prefix, segment] {
        bytes.push(text.len() as u8);
        bytes.extend_from_slice(text.as_bytes());
    }
    bytes.extend_from_slice(room);
    bytes.extend_from_slice(participant);
    bytes
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProofError> {
        self.take(N)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(ProofError::Malformed)
    }

    fn string(&mut self) -> Result<String, ProofError> {
        let [len] = self.array::<1>()?;
        let bytes = self.take(len as usize).ok_or(ProofError::Malformed)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ProofError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn a_proof_round_trips() {
        let proof = Proof::sign("room_prefix", "seg-ment_1", &key(1), &key(2)).unwrap();
        assert_eq!(Proof::decode(&proof.encode()).unwrap(), proof);
    }

    #[test]
    fn a_proof_is_the_same_each_time() {
        let sign = || {
            Proof::sign("room", "seg", &key(1), &key(2))
                .unwrap()
                .encode()
        };
        assert_eq!(sign(), sign());
    }

    #[test]
    fn slashes_around_a_path_dont_matter() {
        let a = Proof::sign("/room/", "seg", &key(1), &key(2)).unwrap();
        let b = Proof::sign("room", "seg", &key(1), &key(2)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_changed_byte_is_refused() {
        let text = Proof::sign("room", "seg", &key(1), &key(2))
            .unwrap()
            .encode();
        let mut bytes = URL_SAFE_NO_PAD.decode(&text).unwrap();
        // The path, then a key, then a signature: each is covered.
        for at in [DOMAIN.len() + 2, bytes.len() - 100, bytes.len() - 1] {
            bytes[at] ^= 1;
            assert!(
                Proof::decode(&URL_SAFE_NO_PAD.encode(&bytes)).is_err(),
                "byte {at}"
            );
            bytes[at] ^= 1;
        }
    }

    #[test]
    fn a_proof_needs_both_keys() {
        // Each signature over the other's message: the participant can't sign as the room.
        let proof = Proof::sign("room", "seg", &key(1), &key(2)).unwrap();
        let mut forged = proof.clone();
        forged.room_signature = proof.participant_signature;
        assert_eq!(Proof::decode(&forged.encode()), Err(ProofError::Signature));
    }

    #[test]
    fn paths_are_plain() {
        for bad in [
            "",
            "a b",
            "a//b",
            "a?b",
            "a*",
            "..%2f",
            &"a".repeat(LIMIT + 1),
        ] {
            assert_eq!(
                Proof::sign(bad, "seg", &key(1), &key(2)),
                Err(ProofError::Path),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = URL_SAFE_NO_PAD
            .decode(
                Proof::sign("room", "seg", &key(1), &key(2))
                    .unwrap()
                    .encode(),
            )
            .unwrap();
        bytes.push(0);
        assert_eq!(
            Proof::decode(&URL_SAFE_NO_PAD.encode(bytes)),
            Err(ProofError::Malformed)
        );
    }
}
