//! Video: pictures in I420, the codecs that carry them, and which codec a track is sent in.
//!
//! Codecs are negotiated, not fixed (D10, design §5.4). Each participant's catalog lists the codecs
//! it can decode, by name ([`Codec::name`]); a sender sends each track in the first codec of its
//! own preference that every other participant can decode ([`choose`]), as Jitsi does; and a
//! participant that can decode none of a sender's codecs is told so. v1 knows one codec, VP9, so
//! the choice is trivial now; a later one arrives without breaking v1 clients, which keep naming
//! only what they decode, and ignore names they don't know.

mod pattern;
#[cfg(feature = "vp9")]
pub mod vp9;

use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};

pub use pattern::{Pattern, PatternCamera, read_pattern};

/// A video codec, as catalogs name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    Vp9,
}

impl Codec {
    /// Every codec this build knows, in its order of preference: best first.
    pub const ALL: [Codec; 1] = [Codec::Vp9];

    /// As catalogs name it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vp9 => "vp9",
        }
    }

    /// The codec a catalog's name names; none for one this build doesn't know.
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|codec| codec.name() == name)
    }
}

/// The codec to send a track in: the first of `preference` that every one of `others` can
/// decode, by the names their catalogs list. None if there's none, and nobody else is no
/// constraint at all: alone in a room, the first.
pub fn choose<'a>(
    preference: &[Codec],
    others: impl IntoIterator<Item = &'a [String]>,
) -> Option<Codec> {
    let others: Vec<&[String]> = others.into_iter().collect();
    preference.iter().copied().find(|codec| {
        others
            .iter()
            .all(|decoders| decoders.iter().any(|name| name == codec.name()))
    })
}

/// A picture in I420: a full-size Y plane, then quarter-size U and V, tightly packed, at even
/// dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Picture {
    /// A picture of `width` by `height`, mid-grey.
    pub fn grey(width: u32, height: u32) -> Self {
        let luma = (width * height) as usize;
        let data = vec![128; luma + luma / 2];
        Self {
            width,
            height,
            data,
        }
    }

    pub fn y(&self) -> &[u8] {
        &self.data[..(self.width * self.height) as usize]
    }

    pub fn y_mut(&mut self) -> &mut [u8] {
        let luma = (self.width * self.height) as usize;
        &mut self.data[..luma]
    }

    /// The U and V planes, each a quarter of Y.
    pub fn uv_mut(&mut self) -> (&mut [u8], &mut [u8]) {
        let luma = (self.width * self.height) as usize;
        let (u, v) = self.data[luma..].split_at_mut(luma / 4);
        (u, v)
    }
}

/// An encoded picture, as a track's frame carries it.
#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    /// Whether it decodes on its own: a group starts with one.
    pub keyframe: bool,
}

/// What encodes pictures in one codec.
pub trait Encoder: Send {
    fn codec(&self) -> Codec;
    /// The packets `picture` encodes to, as a keyframe if `keyframe` asks for one.
    fn encode(&mut self, picture: &Picture, keyframe: bool) -> Result<Vec<Packet>, CodecError>;
}

/// What decodes one codec's packets.
pub trait Decoder: Send {
    fn codec(&self) -> Codec;
    /// The picture `packet` decodes to, if one's ready.
    fn decode(&mut self, packet: &[u8]) -> Result<Option<Picture>, CodecError>;
}

#[derive(Debug, Display, Error)]
#[display("{codec:?}: {why}")]
pub struct CodecError {
    pub codec: Codec,
    #[error(not(source))]
    pub why: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn the_first_codec_everyone_decodes_is_chosen() {
        let bob = names(&["vp9"]);
        let carol = names(&["av2", "vp9"]);
        assert_eq!(
            choose(&Codec::ALL, [bob.as_slice(), carol.as_slice()]),
            Some(Codec::Vp9)
        );
    }

    #[test]
    fn one_who_decodes_nothing_leaves_no_codec() {
        let bob = names(&["vp9"]);
        let dave = names(&[]);
        assert_eq!(choose(&Codec::ALL, [bob.as_slice(), dave.as_slice()]), None);
    }

    #[test]
    fn alone_the_first_is_chosen_and_unknown_names_are_ignored() {
        assert_eq!(choose(&Codec::ALL, std::iter::empty()), Some(Codec::Vp9));
        assert_eq!(Codec::named("av2"), None);
        assert_eq!(Codec::named("vp9"), Some(Codec::Vp9));
    }
}
