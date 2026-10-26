//! Rooms and invites. A room is a stable id and a secret: the secret encrypts everything in the
//! room (D17), the id names the room for the per-room keys (D16), so a later rekey changes the
//! secret without changing anyone's identity. The invite carries both, and is the capability to
//! join (§5.3.1).
//!
//! Two forms of one invite, both versioned from the start:
//!
//! - `mic://join/1/<room>#<secret>`, which opens the app;
//! - `https://<host>/j/#1/<room>/<secret>`, the link the landing page turns into the first
//!   (D25).
//!
//! In both, the secret is in the fragment, which a browser never sends to a server. `<room>` and
//! `<secret>` are base64url without padding.

use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use derive_more::{Display, Error};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The invite format's version: 1. A change that old clients can't read bumps it.
pub const VERSION: u32 = 1;

/// A room's stable id: 16 random bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomId([u8; 16]);

impl RoomId {
    pub fn random() -> Self {
        Self(crate::random())
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RoomId({self})")
    }
}

/// A room's secret: 32 random bytes, from which everything in the room is encrypted. Never
/// printed; wiped when dropped.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct RoomSecret([u8; 32]);

impl RoomSecret {
    pub fn random() -> Self {
        Self(crate::random())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for RoomSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomSecret(…)")
    }
}

/// What it takes to join a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub room: RoomId,
    pub secret: RoomSecret,
}

/// Why a text isn't an invite.
#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum InviteError {
    #[display("not a mic invite: it starts neither with mic://join/ nor with https://…/j/#")]
    NotAnInvite,
    #[display("an invite of format {_0}; this mic reads format {VERSION}: a newer mic made it")]
    Version(#[error(not(source))] String),
    #[display("the invite's {_0} isn't there")]
    Missing(#[error(not(source))] &'static str),
    #[display("the invite's {_0} isn't base64url of the right length")]
    Malformed(#[error(not(source))] &'static str),
}

impl Invite {
    /// A new room, with a fresh id and secret.
    pub fn new_room() -> Self {
        Self {
            room: RoomId::random(),
            secret: RoomSecret::random(),
        }
    }

    /// `mic://join/1/<room>#<secret>`.
    pub fn to_mic_url(&self) -> String {
        format!(
            "mic://join/{VERSION}/{}#{}",
            self.room,
            URL_SAFE_NO_PAD.encode(self.secret.as_bytes())
        )
    }

    /// `https://<host>/j/#1/<room>/<secret>`.
    pub fn to_https_url(&self, host: &str) -> String {
        format!(
            "https://{host}/j/#{VERSION}/{}/{}",
            self.room,
            URL_SAFE_NO_PAD.encode(self.secret.as_bytes())
        )
    }
}

impl FromStr for Invite {
    type Err = InviteError;

    /// Either form, with surrounding whitespace, as pasted.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let (version, room, secret) = if let Some(rest) = text.strip_prefix("mic://join/") {
            let (path, secret) = rest.split_once('#').ok_or(InviteError::Missing("secret"))?;
            let (version, room) = path.split_once('/').ok_or(InviteError::Missing("room"))?;
            (version, room, secret)
        } else if let Some(rest) = text.strip_prefix("https://") {
            let (_host, rest) = rest.split_once('/').ok_or(InviteError::NotAnInvite)?;
            let fragment = rest.strip_prefix("j/#").ok_or(InviteError::NotAnInvite)?;
            let mut parts = fragment.splitn(3, '/');
            let version = parts.next().ok_or(InviteError::Missing("version"))?;
            let room = parts.next().ok_or(InviteError::Missing("room"))?;
            let secret = parts.next().ok_or(InviteError::Missing("secret"))?;
            (version, room, secret)
        } else {
            return Err(InviteError::NotAnInvite);
        };
        if version != VERSION.to_string() {
            return Err(InviteError::Version(version.to_owned()));
        }
        Ok(Self {
            room: RoomId(decode(room, "room")?),
            secret: RoomSecret(decode(secret, "secret")?),
        })
    }
}

/// `text` as base64url without padding, exactly `N` bytes long.
fn decode<const N: usize>(text: &str, what: &'static str) -> Result<[u8; N], InviteError> {
    let mut bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| InviteError::Malformed(what))?;
    let decoded = <[u8; N]>::try_from(bytes.as_slice()).map_err(|_| InviteError::Malformed(what));
    bytes.zeroize();
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite() -> Invite {
        Invite {
            room: RoomId([7; 16]),
            secret: RoomSecret([9; 32]),
        }
    }

    #[test]
    fn both_forms_read_back_as_the_same_invite() {
        let invite = invite();
        assert_eq!(invite.to_mic_url().parse(), Ok(invite.clone()));
        assert_eq!(invite.to_https_url("mic.example").parse(), Ok(invite));
    }

    #[test]
    fn the_secret_is_in_the_fragment() {
        let invite = invite();
        let secret = URL_SAFE_NO_PAD.encode(invite.secret.as_bytes());
        for url in [invite.to_mic_url(), invite.to_https_url("mic.example")] {
            let (before, fragment) = url.split_once('#').unwrap();
            assert!(fragment.contains(&secret));
            assert!(!before.contains(&secret));
        }
    }

    #[test]
    fn a_pasted_invite_may_have_whitespace_around_it() {
        let invite = invite();
        assert_eq!(format!("  {}\n", invite.to_mic_url()).parse(), Ok(invite));
    }

    #[test]
    fn a_newer_format_is_named_as_such() {
        let url = invite().to_mic_url().replace("/join/1/", "/join/2/");
        assert_eq!(url.parse::<Invite>(), Err(InviteError::Version("2".into())));
    }

    #[test]
    fn wrong_texts_are_refused() {
        let room = RoomId([7; 16]).to_string();
        for (text, error) in [
            ("https://mic.example/", InviteError::NotAnInvite),
            ("mic://call/1/x#y", InviteError::NotAnInvite),
            ("mic://join/1/abc", InviteError::Missing("secret")),
            (
                &format!("mic://join/1/{room}#short") as &str,
                InviteError::Malformed("secret"),
            ),
            ("mic://join/1/short#AAAA", InviteError::Malformed("room")),
        ] {
            assert_eq!(text.parse::<Invite>(), Err(error), "{text}");
        }
    }

    #[test]
    fn the_secret_is_never_printed() {
        assert_eq!(format!("{:?}", RoomSecret([9; 32])), "RoomSecret(…)");
        assert!(!format!("{:?}", invite()).contains("CQkJ"));
    }

    #[test]
    fn new_rooms_differ() {
        assert_ne!(Invite::new_room(), Invite::new_room());
    }
}
