//! What the window shows of the room the app is in, as plain data: worked out from the core's
//! state on the runtime, then set on the window on its own thread. Kept apart from Slint's types,
//! so it's tested without a window.

use mic_core::{Connection, RoomState};

/// The screen showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Join,
    Room,
}

impl Screen {
    /// As the window's `screen` property and the harness's `ui` section name it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Room => "room",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    /// The participant's key, hex.
    pub key: String,
    pub name: String,
    pub me: bool,
    /// Whether it says it sends no voice.
    pub muted: bool,
    /// The codec of its camera, when this client can't decode it: the tile says so, rather
    /// than stay black.
    pub cant_decode: Option<String>,
    /// Whether its camera is off: the tile shows the name, not the last picture.
    pub camera_off: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub screen: Screen,
    /// This participant first, then the others in the order they joined.
    pub tiles: Vec<Tile>,
    /// The room's invite, to pass on.
    pub invite: String,
    /// How the session is, when it isn't simply connected.
    pub status: String,
    /// Broadcasts that didn't stand, when there are any.
    pub warning: String,
    /// Whether this participant is muted.
    pub muted: bool,
    /// Whether its camera is off.
    pub camera_off: bool,
}

/// What shows of `room` (its state and its invite), or of none: the join screen.
pub fn view(room: Option<(&RoomState, String)>) -> View {
    let Some((state, invite)) = room else {
        return View {
            screen: Screen::Join,
            tiles: Vec::new(),
            invite: String::new(),
            status: String::new(),
            warning: String::new(),
            muted: false,
            camera_off: false,
        };
    };
    let tile = |participant: &mic_core::Participant, me| Tile {
        key: participant.key.to_string(),
        name: participant.profile.name.clone(),
        me,
        muted: participant.profile.muted,
        camera_off: participant.profile.camera_off,
        cant_decode: participant
            .renditions
            .iter()
            .find(|rendition| !state.me.decoders.contains(&rendition.codec))
            .map(|rendition| rendition.codec.clone()),
    };
    let tiles = std::iter::once(tile(&state.me, true))
        .chain(state.participants.iter().map(|them| tile(them, false)))
        .collect();
    let status = match state.connection {
        Connection::Connected => "",
        Connection::Reconnecting => "Reconnecting to the relay…",
        Connection::Left => "You left the room.",
    };
    let warning = match state.rejected.len() {
        0 => String::new(),
        1 => "Someone in this room publishes what they can't sign. It isn't shown.".to_owned(),
        n => format!(
            "{n} broadcasts in this room aren't signed by who they claim. They aren't shown."
        ),
    };
    View {
        screen: Screen::Room,
        tiles,
        invite,
        status: status.to_owned(),
        warning,
        muted: state.me.profile.muted,
        camera_off: state.me.profile.camera_off,
    }
}

#[cfg(test)]
mod tests {
    use mic_core::roster::Rejected;
    use mic_core::{Participant, ParticipantKey, Profile};

    use super::*;

    fn participant(byte: u8, name: &str) -> Participant {
        Participant {
            key: ParticipantKey::from_bytes([byte; 32]),
            profile: Profile::new(name),
            decoders: vec!["vp9".to_owned()],
            renditions: Vec::new(),
        }
    }

    fn state() -> RoomState {
        RoomState {
            room: "room".to_owned(),
            me: participant(1, "Alice"),
            participants: vec![participant(2, "Bob"), participant(3, "Carol")],
            rejected: Vec::new(),
            connection: Connection::Connected,
            speaker: None,
            speaking: Vec::new(),
        }
    }

    #[test]
    fn no_room_is_the_join_screen() {
        let shown = view(None);
        assert_eq!(shown.screen, Screen::Join);
        assert!(shown.tiles.is_empty());
    }

    #[test]
    fn a_room_shows_me_first_then_the_others_in_order() {
        let shown = view(Some((&state(), "mic://join/1/x#y".to_owned())));
        assert_eq!(shown.screen, Screen::Room);
        let names: Vec<_> = shown.tiles.iter().map(|tile| tile.name.as_str()).collect();
        assert_eq!(names, ["Alice", "Bob", "Carol"]);
        assert!(shown.tiles[0].me && !shown.tiles[1].me);
        assert_eq!(shown.tiles[1].key, "02".repeat(32));
        assert_eq!(shown.invite, "mic://join/1/x#y");
        assert_eq!(shown.status, "");
        assert_eq!(shown.warning, "");
    }

    #[test]
    fn who_is_muted_shows_on_their_tile() {
        let mut state = state();
        state.me.profile.muted = true;
        state.participants[1].profile.muted = true;
        let shown = view(Some((&state, String::new())));
        assert!(shown.muted);
        let muted: Vec<bool> = shown.tiles.iter().map(|tile| tile.muted).collect();
        assert_eq!(muted, [true, false, true]);
    }

    #[test]
    fn a_camera_this_client_cant_decode_is_said_on_its_tile() {
        let mut state = state();
        state.me.decoders.clear();
        state.participants[0].renditions = vec![mic_core::catalog::Rendition {
            track: "camera".to_owned(),
            codec: "vp9".to_owned(),
        }];
        let shown = view(Some((&state, String::new())));
        assert_eq!(shown.tiles[1].cant_decode.as_deref(), Some("vp9"));
        assert_eq!(shown.tiles[2].cant_decode, None, "Carol sends no camera");
    }

    #[test]
    fn a_camera_turned_off_shows_on_its_tile() {
        let mut state = state();
        state.participants[0].profile.camera_off = true;
        let shown = view(Some((&state, String::new())));
        assert!(shown.tiles[1].camera_off && !shown.tiles[2].camera_off);
    }

    #[test]
    fn reconnecting_and_rejected_broadcasts_are_said() {
        let mut state = state();
        state.connection = Connection::Reconnecting;
        state.rejected = vec![
            Rejected {
                path: "a".to_owned(),
                reason: "forged".to_owned(),
            };
            2
        ];
        let shown = view(Some((&state, String::new())));
        assert!(shown.status.contains("Reconnecting"));
        assert!(shown.warning.starts_with("2 broadcasts"));
    }
}
