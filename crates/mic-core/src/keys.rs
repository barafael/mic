//! Keys: a device secret, and from it one Ed25519 key per room (D16). The device secret is 32
//! random bytes, kept in the system keyring (the next commit); here it's passed in.
//!
//! `room_key = HKDF-SHA256(ikm = device_secret, info = "mic/v1/room-identity" || room_id)`, taken
//! as an Ed25519 seed. So a reconnect keeps the identity with nothing stored per room, and rooms
//! can't be linked without the device secret.

use std::fmt;

use ed25519_dalek::{SigningKey, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::invite::RoomId;

/// The HKDF info's prefix for a room's key: versioned, so another derivation never collides.
const ROOM_IDENTITY: &[u8] = b"mic/v1/room-identity";

/// A device's secret: 32 random bytes from the OS, made once. Never printed; wiped when dropped.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct DeviceSecret([u8; 32]);

impl DeviceSecret {
    pub fn random() -> Self {
        Self(crate::random())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// This device's key in `room`.
    pub fn room_key(&self, room: &RoomId) -> RoomKey {
        let mut info = Vec::with_capacity(ROOM_IDENTITY.len() + room.as_bytes().len());
        info.extend_from_slice(ROOM_IDENTITY);
        info.extend_from_slice(room.as_bytes());
        let mut seed = [0; 32];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(&info, &mut seed)
            .expect("32 bytes is a valid length for HKDF-SHA256");
        let key = SigningKey::from_bytes(&seed);
        seed.zeroize();
        RoomKey(key)
    }
}

impl fmt::Debug for DeviceSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceSecret(…)")
    }
}

/// A participant's key in one room: it signs what the participant publishes there (D18).
#[derive(Clone)]
pub struct RoomKey(SigningKey);

impl RoomKey {
    pub fn signing(&self) -> &SigningKey {
        &self.0
    }

    /// What others know the participant by in the room.
    pub fn public(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

impl fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RoomKey({:?})", self.public())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_has_one_key_per_room() {
        let device = DeviceSecret::from_bytes([1; 32]);
        let room = RoomId::from_bytes([2; 16]);
        assert_eq!(
            device.room_key(&room).public(),
            device.room_key(&room).public()
        );
        assert_ne!(
            device.room_key(&room).public(),
            device.room_key(&RoomId::from_bytes([3; 16])).public()
        );
        assert_ne!(
            device.room_key(&room).public(),
            DeviceSecret::from_bytes([4; 32]).room_key(&room).public()
        );
    }

    /// The derivation is part of the wire format: participants recognise each other across
    /// reconnects and versions by it. A change here must be deliberate, with the info's version.
    #[test]
    fn the_derivation_is_fixed() {
        let key = DeviceSecret::from_bytes([1; 32]).room_key(&RoomId::from_bytes([2; 16]));
        let public: String = key
            .public()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(public, KNOWN);
    }

    /// Computed apart from this code: HKDF-SHA256 by hand in Python, the Ed25519 public key by
    /// OpenSSL from the seed.
    const KNOWN: &str = "9c89eeef159d671ac010cbef04d8fb603f5a9f19430460ff0d2a85d30e034ffb";

    #[test]
    fn secrets_are_never_printed() {
        assert_eq!(format!("{:?}", DeviceSecret([1; 32])), "DeviceSecret(…)");
    }
}
