//! Rooms and invites. A room is a stable id and a secret: the secret encrypts everything in the
//! room (D17), the id names the room for the per-room keys (D16), so a later rekey changes the
//! secret without changing anyone's identity. The invite carries both, and is the capability to
//! join (§5.3.1).
//!
//! Two forms of one invite, both versioned from the start:
//!
//! - `mic://join/1/<room>#<secret>[/<name>]`, which opens the app;
//! - `https://<host>/j/#1/<room>/<secret>[/<name>]`, the link the landing page turns into the
//!   first (D25).
//!
//! In both, the secret is in the fragment, which a browser never sends to a server. `<room>` and
//! `<secret>` are base64url without padding; `<name>`, what the room is called, if it's called
//! anything, is its UTF-8 in base64url, at most [`NAME_LIMIT`] characters. What comes after the
//! name is read past: a later field a client without it ignores.

use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use derive_more::{Display, Error};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The invite format's version: 1. A change that old clients can't read bumps it.
pub const VERSION: u32 = 1;
/// How long a room's name may be, in characters.
pub const NAME_LIMIT: usize = 64;

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
    /// What the room is called, if anything: shown to who joins, never sent to the relay.
    pub name: Option<String>,
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
            name: None,
        }
    }

    /// The same room, called `name`: trimmed, cut to [`NAME_LIMIT`] characters, and none if
    /// that leaves nothing.
    pub fn named(self, name: &str) -> Self {
        let name: String = name.trim().chars().take(NAME_LIMIT).collect();
        Self {
            name: (!name.is_empty()).then_some(name),
            ..self
        }
    }

    /// The name's part of either form: `/<name>`, or nothing.
    fn name_part(&self) -> String {
        self.name
            .as_ref()
            .map(|name| format!("/{}", URL_SAFE_NO_PAD.encode(name.as_bytes())))
            .unwrap_or_default()
    }

    /// `mic://join/1/<room>#<secret>`.
    pub fn to_mic_url(&self) -> String {
        format!(
            "mic://join/{VERSION}/{}#{}{}",
            self.room,
            URL_SAFE_NO_PAD.encode(self.secret.as_bytes()),
            self.name_part()
        )
    }

    /// `https://<host>/j/#1/<room>/<secret>`.
    pub fn to_https_url(&self, host: &str) -> String {
        format!(
            "https://{host}/j/#{VERSION}/{}/{}{}",
            self.room,
            URL_SAFE_NO_PAD.encode(self.secret.as_bytes()),
            self.name_part()
        )
    }
}

impl FromStr for Invite {
    type Err = InviteError;

    /// Either form, with surrounding whitespace, as pasted.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let (version, room, after) = if let Some(rest) = text.strip_prefix("mic://join/") {
            let (path, after) = rest.split_once('#').ok_or(InviteError::Missing("secret"))?;
            let (version, room) = path.split_once('/').ok_or(InviteError::Missing("room"))?;
            (version, room, after)
        } else if let Some(rest) = text.strip_prefix("https://") {
            let (_host, rest) = rest.split_once('/').ok_or(InviteError::NotAnInvite)?;
            let fragment = rest.strip_prefix("j/#").ok_or(InviteError::NotAnInvite)?;
            let mut parts = fragment.splitn(3, '/');
            let version = parts.next().ok_or(InviteError::Missing("version"))?;
            let room = parts.next().ok_or(InviteError::Missing("room"))?;
            let after = parts.next().ok_or(InviteError::Missing("secret"))?;
            (version, room, after)
        } else {
            return Err(InviteError::NotAnInvite);
        };
        // The secret, then the name, if there's one; anything after them is a later field's.
        let mut after = after.split('/');
        let secret = after.next().unwrap_or_default();
        let name = match after.next().filter(|name| !name.is_empty()) {
            Some(name) => {
                let bytes = URL_SAFE_NO_PAD
                    .decode(name)
                    .map_err(|_| InviteError::Malformed("name"))?;
                let name = String::from_utf8(bytes).map_err(|_| InviteError::Malformed("name"))?;
                Some(name)
            }
            None => None,
        };
        if version != VERSION.to_string() {
            return Err(InviteError::Version(version.to_owned()));
        }
        let invite = Self {
            room: RoomId(decode(room, "room")?),
            secret: RoomSecret(decode(secret, "secret")?),
            name: None,
        };
        Ok(match name {
            Some(name) => invite.named(&name),
            None => invite,
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
            name: None,
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
    fn a_name_reads_back_in_both_forms_and_a_later_field_is_read_past() {
        let named = invite().named("  Friday's standup ");
        assert_eq!(named.name.as_deref(), Some("Friday's standup"));
        assert_eq!(named.to_mic_url().parse(), Ok(named.clone()));
        assert_eq!(named.to_https_url("mic.example").parse(), Ok(named.clone()));
        // Still in the fragment, as the secret is.
        let https = named.to_https_url("mic.example");
        assert!(
            https
                .split_once('#')
                .unwrap()
                .1
                .ends_with(&URL_SAFE_NO_PAD.encode("Friday's standup"))
        );
        assert_eq!(format!("{}/later", named.to_mic_url()).parse(), Ok(named));
        // No name, an empty one, and one that isn't a name.
        assert_eq!(invite().named("   ").name, None);
        assert_eq!(format!("{}/", invite().to_mic_url()).parse(), Ok(invite()));
        assert_eq!(
            format!("{}/!!", invite().to_mic_url()).parse::<Invite>(),
            Err(InviteError::Malformed("name"))
        );
        let long = invite().named(&"x".repeat(NAME_LIMIT + 10));
        assert_eq!(long.name.map(|name| name.chars().count()), Some(NAME_LIMIT));
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

    /// The invite site/invite.test.mjs holds the landing page to: both forms, as mic writes them.
    #[test]
    fn the_landing_pages_fixture() {
        let invite = Invite {
            room: RoomId([1; 16]),
            secret: RoomSecret([2; 32]),
            name: None,
        };
        let (room, secret) = (
            "AQEBAQEBAQEBAQEBAQEBAQ",
            "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI",
        );
        assert_eq!(
            invite.to_https_url("example.org"),
            format!("https://example.org/j/#1/{room}/{secret}")
        );
        assert_eq!(invite.to_mic_url(), format!("mic://join/1/{room}#{secret}"));
    }
}
