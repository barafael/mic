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
    /// Whether it's speaking: its tile is ringed. Nothing says so yet: the active speaker comes
    /// with selective subscriptions (PLAN.md, commit 43).
    pub speaking: bool,
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
    /// Whether it has a camera that's sent: without one, the camera's button is disabled. The
    /// room knows, not its state (`Room::preview`), so the view's maker sets it.
    pub camera: bool,
    /// The participants not shown beyond the tiles, as a last "+N" tile. None are left out yet:
    /// which tiles show comes with selective subscriptions (PLAN.md, commit 43).
    pub overflow: usize,
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
            camera: false,
            overflow: 0,
        };
    };
    let tile = |participant: &mic_core::Participant, me| Tile {
        key: participant.key.to_string(),
        name: participant.profile.name.clone(),
        me,
        muted: participant.profile.muted,
        camera_off: participant.profile.camera_off,
        speaking: false,
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
        camera: false,
        overflow: 0,
    }
}

/// How many columns show `count` tiles largest in a `width` by `height` grid, `gap` apart, each
/// tile at `aspect` (width over height) as the cameras send. Of layouts within a few percent of
/// the largest, the one shaped like the grid: side by side in a wide window, stacked in a tall
/// one.
pub fn columns(count: usize, width: f32, height: f32, gap: f32, aspect: f32) -> usize {
    let count = count.max(1);
    let size = |columns: usize| {
        let rows = count.div_ceil(columns);
        let cell_width = (width - gap * (columns - 1) as f32) / columns as f32;
        let cell_height = (height - gap * (rows - 1) as f32) / rows as f32;
        cell_width.min(cell_height * aspect)
    };
    let largest = (1..=count).map(size).fold(0.0, f32::max);
    let near = (1..=count).filter(|&columns| size(columns) >= largest * 0.95);
    let chosen = if width >= height {
        near.max()
    } else {
        near.min()
    };
    chosen.unwrap_or(1)
}

/// What stands for `name` on a tile without video: the first letters of its first two words.
pub fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "?".to_owned()
    } else {
        letters
    }
}

/// A participant's colour, the same on every client: one of a few that white letters read on,
/// chosen by its key (hex).
pub fn tint(key: &str) -> (u8, u8, u8) {
    const TINTS: [(u8, u8, u8); 8] = [
        (0x3b, 0x6e, 0xd8),
        (0x7c, 0x4d, 0xc4),
        (0xb8, 0x45, 0x8a),
        (0xc2, 0x5b, 0x34),
        (0x2f, 0x8f, 0x6a),
        (0x2b, 0x84, 0x9e),
        (0x8a, 0x6d, 0x2a),
        (0x5a, 0x63, 0x78),
    ];
    let byte = u8::from_str_radix(key.get(..2).unwrap_or("00"), 16).unwrap_or(0);
    TINTS[usize::from(byte) % TINTS.len()]
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
    fn tiles_fill_the_grid_as_large_as_they_can() {
        let columns = |count, width, height| columns(count, width, height, 8.0, 16.0 / 9.0);
        // Side by side in a wide window, one above the other in a tall one.
        assert_eq!(columns(2, 1000.0, 400.0), 2);
        assert_eq!(columns(2, 400.0, 1000.0), 1);
        assert_eq!(columns(1, 1000.0, 600.0), 1);
        // Two in a landscape window that would fit them as large stacked: side by side.
        assert_eq!(columns(2, 1076.0, 612.0), 2);
        // Five in a 16:10 window: three and two, not five in a row.
        assert_eq!(columns(5, 1100.0, 600.0), 3);
        assert_eq!(columns(9, 1280.0, 720.0), 3);
        assert_eq!(columns(0, 0.0, 0.0), 1);
    }

    #[test]
    fn initials_are_of_the_first_two_words() {
        assert_eq!(initials("Ada"), "A");
        assert_eq!(initials("ada lovelace byron"), "AL");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("(Bob) Smith"), "BS");
    }

    #[test]
    fn a_tint_is_the_same_for_a_key() {
        assert_eq!(tint(&"02".repeat(32)), tint(&"02".repeat(32)));
        assert_ne!(tint(&"02".repeat(32)), tint(&"03".repeat(32)));
        assert_eq!(tint(""), tint("00"));
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
