//! A participant's signed catalog (D18, step 1): the first track of its broadcast, encrypted like
//! every track, saying who publishes it and what's in it, signed with the publisher's key in the
//! room (§5.3.2).
//!
//! Every member holds the room's secret, so any member can encrypt under any broadcast's names;
//! what only the publisher can do is sign with its key. A catalog verifies only if:
//!
//! - its signature checks against the key it names;
//! - the broadcast's participant segment is the one that key derives (`RoomCrypto::participant`),
//!   so a catalog signed with one key can't stand at another participant's path;
//! - its room, key id and epoch are the broadcast's;
//! - its sequence number is above the last one verified, so an old catalog can't be replayed.
//!
//! The catalog is canonical bytes, not JSON, so what's signed is exactly what's sent:
//!
//! ```text
//! "mic/v1/catalog\0" format:u32 room:16 participant:32 kid:u64 epoch:str seq:u64
//!                    tracks:u16 (track:str)* decoders:u16 (codec:str)*
//!                    renditions:u16 (track:str codec:str)* app:str signature:64
//! ```
//!
//! `decoders` are the video codecs the participant can decode, `renditions` the codec each of its
//! video tracks is sent in, both by name (mic-media's `Codec::name`): a newer client may name a
//! codec an older one doesn't know, which the older one only can't decode (D10, §5.4).
//!
//! Integers big-endian; `str` a u16 length and UTF-8.

use std::fmt;

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};

use crate::invite::RoomId;
use crate::keys::RoomKey;
use crate::roster::ParticipantKey;

/// The catalog track's semantic name.
pub const CATALOG: &str = "catalog";
/// What every catalog starts with: a signature over it can't stand for another kind of message.
const DOMAIN: &[u8] = b"mic/v1/catalog\0";
/// The catalog's format: a change older clients can't read bumps it.
const FORMAT: u32 = 1;
/// An Ed25519 signature's length.
const SIGNATURE: usize = 64;

/// What a participant's broadcast says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub room: RoomId,
    pub participant: ParticipantKey,
    /// The room credential's key id (`crate::tracks`).
    pub kid: u64,
    /// The publisher instance's epoch, which ends the broadcast's path.
    pub epoch: String,
    /// Up by one with every new catalog of the broadcast.
    pub seq: u64,
    /// The broadcast's tracks, by semantic name.
    pub tracks: Vec<String>,
    /// The video codecs the participant decodes, by name.
    pub decoders: Vec<String>,
    /// Which codec each of its video tracks is sent in.
    pub renditions: Vec<Rendition>,
    /// The app's version, for "a participant is on a newer version" (§5.12).
    pub app: String,
}

/// A video track, and the codec it's sent in, by name.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rendition {
    pub track: String,
    pub codec: String,
}

/// Why a broadcast's catalog doesn't stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Not a catalog of this format: cut short, or of a newer format.
    Malformed(String),
    /// The signature doesn't check against the key the catalog names.
    Signature,
    /// The catalog names a key whose path isn't the broadcast's: someone else's catalog, or a
    /// catalog naming someone else.
    NotItsPath,
    /// Of another room, key id or publisher instance.
    Elsewhere(&'static str),
    /// A sequence number not above the last one verified: a replayed catalog.
    Replayed { seq: u64, last: u64 },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "a catalog that doesn't read: {why}"),
            Self::Signature => f.write_str("a catalog whose signature doesn't check"),
            Self::NotItsPath => f.write_str("a catalog naming a participant whose path this isn't"),
            Self::Elsewhere(what) => write!(f, "a catalog of another {what}"),
            Self::Replayed { seq, last } => {
                write!(f, "an old catalog again: number {seq}, after {last}")
            }
        }
    }
}

/// What a catalog must match: the broadcast it was found at.
pub struct Expected<'a> {
    pub room: RoomId,
    pub kid: u64,
    pub epoch: &'a str,
    /// The broadcast's participant segment, and how a key derives one.
    pub participant_segment: &'a str,
    pub segment_of: &'a (dyn Fn(&ParticipantKey) -> Option<String> + Sync),
    /// The last sequence number verified for the broadcast, if any.
    pub last: Option<u64>,
}

impl Catalog {
    /// The catalog's bytes, signed with `key`, which must be the one it names.
    pub fn sign(&self, key: &RoomKey) -> Vec<u8> {
        debug_assert_eq!(self.participant.as_bytes(), key.public().as_bytes());
        let mut bytes = self.encode();
        let signature = key.signing().sign(&bytes);
        bytes.extend_from_slice(&signature.to_bytes());
        bytes
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend_from_slice(&FORMAT.to_be_bytes());
        bytes.extend_from_slice(self.room.as_bytes());
        bytes.extend_from_slice(self.participant.as_bytes());
        bytes.extend_from_slice(&self.kid.to_be_bytes());
        put_str(&mut bytes, &self.epoch);
        bytes.extend_from_slice(&self.seq.to_be_bytes());
        let count = u16::try_from(self.tracks.len()).expect("fewer than 65536 tracks");
        bytes.extend_from_slice(&count.to_be_bytes());
        for track in &self.tracks {
            put_str(&mut bytes, track);
        }
        put_count(&mut bytes, self.decoders.len());
        for codec in &self.decoders {
            put_str(&mut bytes, codec);
        }
        put_count(&mut bytes, self.renditions.len());
        for rendition in &self.renditions {
            put_str(&mut bytes, &rendition.track);
            put_str(&mut bytes, &rendition.codec);
        }
        put_str(&mut bytes, &self.app);
        bytes
    }

    /// Whom the catalog `bytes` names, whether it stands or not: who a broadcast that doesn't
    /// claims to be.
    pub fn claimed(bytes: &[u8]) -> Option<ParticipantKey> {
        let at = DOMAIN.len() + 4 + 16;
        let key: [u8; 32] = bytes.get(at..at + 32)?.try_into().ok()?;
        bytes
            .starts_with(DOMAIN)
            .then_some(ParticipantKey::from_bytes(key))
    }

    /// The catalog `bytes` are, if it stands for the broadcast `expected` describes.
    pub fn verify(bytes: &[u8], expected: &Expected<'_>) -> Result<Self, Rejection> {
        let malformed = |why: &str| Rejection::Malformed(why.to_owned());
        if bytes.len() < DOMAIN.len() + SIGNATURE {
            return Err(malformed("too short"));
        }
        let (signed, signature) = bytes.split_at(bytes.len() - SIGNATURE);
        let mut reader = Reader(signed);
        if reader.take(DOMAIN.len())? != DOMAIN {
            return Err(malformed("not a catalog"));
        }
        let format = u32::from_be_bytes(reader.array()?);
        if format != FORMAT {
            return Err(Rejection::Malformed(format!(
                "of format {format}, this mic reads {FORMAT}"
            )));
        }
        let catalog = Self {
            room: RoomId::from_bytes(reader.array()?),
            participant: ParticipantKey::from_bytes(reader.array()?),
            kid: u64::from_be_bytes(reader.array()?),
            epoch: reader.str()?,
            seq: u64::from_be_bytes(reader.array()?),
            tracks: {
                let count = u16::from_be_bytes(reader.array()?);
                (0..count).map(|_| reader.str()).collect::<Result<_, _>>()?
            },
            decoders: {
                let count = u16::from_be_bytes(reader.array()?);
                (0..count).map(|_| reader.str()).collect::<Result<_, _>>()?
            },
            renditions: {
                let count = u16::from_be_bytes(reader.array()?);
                (0..count)
                    .map(|_| {
                        Ok(Rendition {
                            track: reader.str()?,
                            codec: reader.str()?,
                        })
                    })
                    .collect::<Result<_, Rejection>>()?
            },
            app: reader.str()?,
        };
        if !reader.0.is_empty() {
            return Err(malformed("bytes after its end"));
        }
        let key = VerifyingKey::from_bytes(catalog.participant.as_bytes())
            .map_err(|_| Rejection::Signature)?;
        let signature = Signature::from_slice(signature).map_err(|_| Rejection::Signature)?;
        key.verify(signed, &signature)
            .map_err(|_| Rejection::Signature)?;
        if (expected.segment_of)(&catalog.participant).as_deref()
            != Some(expected.participant_segment)
        {
            return Err(Rejection::NotItsPath);
        }
        if catalog.room != expected.room {
            return Err(Rejection::Elsewhere("room"));
        }
        if catalog.kid != expected.kid {
            return Err(Rejection::Elsewhere("key id"));
        }
        if catalog.epoch != expected.epoch {
            return Err(Rejection::Elsewhere("publisher instance"));
        }
        if let Some(last) = expected.last
            && catalog.seq <= last
        {
            return Err(Rejection::Replayed {
                seq: catalog.seq,
                last,
            });
        }
        Ok(catalog)
    }
}

fn put_count(bytes: &mut Vec<u8>, count: usize) {
    let count = u16::try_from(count).expect("fewer than 65536 of a catalog's entries");
    bytes.extend_from_slice(&count.to_be_bytes());
}

fn put_str(bytes: &mut Vec<u8>, text: &str) {
    let length = u16::try_from(text.len()).expect("a catalog's text is shorter than 64 KiB");
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
}

/// The bytes of a catalog still to read.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Rejection> {
        if self.0.len() < count {
            return Err(Rejection::Malformed("cut short".to_owned()));
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Rejection> {
        Ok(self.take(N)?.try_into().expect("taken N bytes"))
    }

    fn str(&mut self) -> Result<String, Rejection> {
        let length = u16::from_be_bytes(self.array()?);
        let text = self.take(usize::from(length))?;
        String::from_utf8(text.to_vec())
            .map_err(|_| Rejection::Malformed("text that isn't UTF-8".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::DeviceSecret;

    fn key(device: u8) -> RoomKey {
        DeviceSecret::from_bytes([device; 32]).room_key(&RoomId::from_bytes([1; 16]))
    }

    fn catalog(key: &RoomKey) -> Catalog {
        Catalog {
            room: RoomId::from_bytes([1; 16]),
            participant: ParticipantKey::from_bytes(key.public().to_bytes()),
            kid: 0,
            epoch: "01a17a43-1786-730f-a3b3-15083bc6f37b".to_owned(),
            seq: 3,
            tracks: vec!["catalog".to_owned(), "profile".to_owned()],
            decoders: vec!["vp9".to_owned(), "av2".to_owned()],
            renditions: vec![Rendition {
                track: "camera".to_owned(),
                codec: "vp9".to_owned(),
            }],
            app: "0.1.0".to_owned(),
        }
    }

    /// A key's participant segment, as the test has it: its hex, reversed.
    fn segment(key: &ParticipantKey) -> Option<String> {
        Some(key.to_string().chars().rev().collect())
    }

    fn expected<'a>(participant: &'a str, last: Option<u64>) -> Expected<'a> {
        Expected {
            room: RoomId::from_bytes([1; 16]),
            kid: 0,
            epoch: "01a17a43-1786-730f-a3b3-15083bc6f37b",
            participant_segment: participant,
            segment_of: &segment,
            last,
        }
    }

    #[test]
    fn a_signed_catalog_verifies_at_its_own_path() {
        let alice = key(1);
        let catalog = catalog(&alice);
        let path = segment(&catalog.participant).unwrap();
        assert_eq!(
            Catalog::verify(&catalog.sign(&alice), &expected(&path, Some(2))),
            Ok(catalog)
        );
    }

    #[test]
    fn a_changed_byte_is_refused() {
        let alice = key(1);
        let catalog = catalog(&alice);
        let path = segment(&catalog.participant).unwrap();
        let signed = catalog.sign(&alice);
        for at in DOMAIN.len()..signed.len() {
            let mut tampered = signed.clone();
            tampered[at] ^= 1;
            assert!(
                Catalog::verify(&tampered, &expected(&path, None)).is_err(),
                "byte {at} changed, and it verified"
            );
        }
    }

    #[test]
    fn a_catalog_at_someone_elses_path_is_refused() {
        // Mallory signs a catalog with her own key, at Alice's path.
        let alice = key(1);
        let mallory = key(2);
        let alices = segment(&catalog(&alice).participant).unwrap();
        let signed = catalog(&mallory).sign(&mallory);
        assert_eq!(
            Catalog::verify(&signed, &expected(&alices, None)),
            Err(Rejection::NotItsPath)
        );
        // And a catalog naming Alice, signed by Mallory, doesn't check at all.
        let mut naming_alice = catalog(&alice).encode();
        naming_alice.extend_from_slice(&mallory.signing().sign(&naming_alice).to_bytes());
        assert_eq!(
            Catalog::verify(&naming_alice, &expected(&alices, None)),
            Err(Rejection::Signature)
        );
    }

    #[test]
    fn an_old_catalog_or_one_of_another_instance_is_refused() {
        let alice = key(1);
        let catalog = catalog(&alice);
        let path = segment(&catalog.participant).unwrap();
        let signed = catalog.sign(&alice);
        assert_eq!(
            Catalog::verify(&signed, &expected(&path, Some(3))),
            Err(Rejection::Replayed { seq: 3, last: 3 })
        );
        let mut elsewhere = expected(&path, None);
        elsewhere.epoch = "01a17a43-0000-730f-a3b3-15083bc6f37b";
        assert_eq!(
            Catalog::verify(&signed, &elsewhere),
            Err(Rejection::Elsewhere("publisher instance"))
        );
    }

    #[test]
    fn what_isnt_a_catalog_is_malformed() {
        let path = "x";
        for bytes in [&b""[..], &[0; 80][..]] {
            assert!(matches!(
                Catalog::verify(bytes, &expected(path, None)),
                Err(Rejection::Malformed(_))
            ));
        }
    }
}
