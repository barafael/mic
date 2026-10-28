//! What the window shows of the room the app is in, as plain data: worked out from the core's
//! state on the runtime, then set on the window on its own thread. Kept apart from Slint's types,
//! so it's tested without a window.

use std::collections::HashMap;

use mic_core::{Connection, ParticipantKey, RoomState};

/// How many tiles show at most, this participant's among them: the rest are a "+N" tile, and
/// their cameras aren't subscribed to (§5.8).
pub const MOST_TILES: usize = 9;

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
    /// Whether it's speaking: its tile is ringed.
    pub speaking: bool,
    /// Whether its hand is raised.
    pub hand: bool,
    /// Its latest reaction, and which of the room's it was, which the tile shows a moment.
    pub reaction: Option<(mic_core::chat::Reaction, u64)>,
}

/// A chat message, as the panel shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Who said it, by key, hex: a run of one sender's messages shows the name once.
    pub from: String,
    pub name: String,
    pub text: String,
    pub me: bool,
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
    /// The participants not shown beyond the tiles, as a last "+N" tile.
    pub overflow: usize,
    /// Whether this participant's hand is raised.
    pub hand: bool,
    /// The chat, oldest first.
    pub chat: Vec<Line>,
    /// The others sharing a screen, which the stage shows, in the order they joined.
    pub screens: Vec<Tile>,
    /// Whether this participant shares its screen.
    pub sharing: bool,
    /// Who records the room, by key, this participant among them if it does (D7).
    pub recording: Vec<String>,
    /// What the window says of it: who records, or nothing.
    pub recording_notice: String,
    /// Whether this participant records.
    pub recording_self: bool,
    /// Whether a share of its screen is opening: the desktop may be asking which.
    pub share_opening: bool,
}

/// Which of the others have a tile, when there are more than fit: the active speaker, and those
/// who spoke most recently, then those who joined first. Kept from one view to the next, as it
/// remembers who spoke when.
#[derive(Default)]
pub struct Shown {
    spoke: HashMap<ParticipantKey, u64>,
    views: u64,
}

impl Shown {
    /// The others shown in `state`, in the order they joined.
    pub fn choose(&mut self, state: &RoomState) -> Vec<ParticipantKey> {
        self.views += 1;
        // The speaker counts as speaking last, now.
        for key in state.speaking.iter().chain(&state.speaker) {
            self.spoke.insert(*key, self.views);
        }
        self.spoke
            .retain(|key, _| state.participants.iter().any(|them| them.key == *key));
        let room = MOST_TILES - 1;
        let mut ranked: Vec<(usize, ParticipantKey)> = state
            .participants
            .iter()
            .map(|them| them.key)
            .enumerate()
            .collect();
        if ranked.len() > room {
            ranked.sort_by_key(|(joined, key)| {
                let last = if state.speaker == Some(*key) {
                    u64::MAX
                } else {
                    self.spoke.get(key).copied().unwrap_or(0)
                };
                (std::cmp::Reverse(last), *joined)
            });
            ranked.truncate(room);
            ranked.sort();
        }
        ranked.into_iter().map(|(_, key)| key).collect()
    }
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
            hand: false,
            chat: Vec::new(),
            screens: Vec::new(),
            sharing: false,
            recording: Vec::new(),
            recording_notice: String::new(),
            recording_self: false,
            share_opening: false,
        };
    };
    let first = Shown::default().choose(state);
    view_of(state, invite, &first)
}

/// What shows of `state`, the room of `invite`, the others `shown` given tiles.
pub fn view_of(state: &RoomState, invite: String, shown: &[ParticipantKey]) -> View {
    let tile = |participant: &mic_core::Participant, me| Tile {
        key: participant.key.to_string(),
        name: participant.profile.name.clone(),
        me,
        muted: participant.profile.muted,
        camera_off: participant.profile.camera_off,
        speaking: state.speaking.contains(&participant.key),
        hand: participant.profile.hand,
        reaction: state
            .reactions
            .iter()
            .rev()
            .find(|reacted| reacted.from == participant.key)
            .map(|reacted| (reacted.reaction, reacted.seq)),
        cant_decode: participant
            .renditions
            .iter()
            .find(|rendition| !state.me.decoders.contains(&rendition.codec))
            .map(|rendition| rendition.codec.clone()),
    };
    let tiles = std::iter::once(tile(&state.me, true))
        .chain(
            state
                .participants
                .iter()
                .filter(|them| shown.contains(&them.key))
                .map(|them| tile(them, false)),
        )
        .collect();
    let chat = state
        .chat
        .iter()
        .map(|message| Line {
            from: message.from.to_string(),
            name: message.name.clone(),
            text: message.text.clone(),
            me: message.from == state.me.key,
        })
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
        overflow: state.participants.len().saturating_sub(shown.len()),
        hand: state.me.profile.hand,
        chat,
        screens: state
            .participants
            .iter()
            .filter(|them| them.profile.sharing)
            .map(|them| Tile {
                name: format!("{}'s screen", them.profile.name),
                speaking: false,
                hand: false,
                reaction: None,
                ..tile(them, false)
            })
            .collect(),
        sharing: state.me.profile.sharing,
        recording: std::iter::once(&state.me)
            .chain(&state.participants)
            .filter(|them| them.profile.recording)
            .map(|them| them.key.to_string())
            .collect(),
        recording_notice: recording_notice(state),
        recording_self: state.me.profile.recording,
        share_opening: state.screen_opening,
    }
}

/// Who records `state`'s room, as the window says it.
fn recording_notice(state: &RoomState) -> String {
    let others: Vec<&str> = state
        .participants
        .iter()
        .filter(|them| them.profile.recording)
        .map(|them| them.profile.name.as_str())
        .collect();
    let others = match others.as_slice() {
        [] => None,
        [one] => Some(format!("{one} is recording this call.")),
        [rest @ .., last] => Some(format!(
            "{} and {last} are recording this call.",
            rest.join(", ")
        )),
    };
    match (state.me.profile.recording, others) {
        (false, None) => String::new(),
        (true, None) => "You're recording this call; everyone sees that you are.".to_owned(),
        (true, Some(others)) => format!("You're recording this call. {others}"),
        (false, Some(others)) => others,
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
            ..Default::default()
        }
    }

    fn state() -> RoomState {
        RoomState {
            room: "room".to_owned(),
            me: participant(1, "Alice"),
            participants: vec![participant(2, "Bob"), participant(3, "Carol")],
            ..Default::default()
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
                posing_as: None,
            };
            2
        ];
        let shown = view(Some((&state, String::new())));
        assert!(shown.status.contains("Reconnecting"));
        assert!(shown.warning.starts_with("2 broadcasts"));
    }

    #[test]
    fn beyond_nine_tiles_those_who_spoke_last_show_and_the_rest_are_counted() {
        let mut state = state();
        state.participants = (2..=13)
            .map(|byte| participant(byte, &format!("P{byte}")))
            .collect();
        let key = |byte: u8| ParticipantKey::from_bytes([byte; 32]);
        let mut shown = Shown::default();
        // Nobody spoke: the first to join.
        let first = shown.choose(&state);
        assert_eq!(first, (2..=9).map(key).collect::<Vec<_>>());
        let view = view_of(&state, String::new(), &first);
        assert_eq!((view.tiles.len(), view.overflow), (MOST_TILES, 4));
        // The last to join speaks: in, in join order, and the one who joined last before it out.
        state.speaking = vec![key(13)];
        state.speaker = Some(key(13));
        let spoken = shown.choose(&state);
        assert!(spoken.contains(&key(13)) && !spoken.contains(&key(9)));
        assert_eq!(spoken.last(), Some(&key(13)));
        // They stop; someone else not shown speaks: both stay, the speaker first among them.
        state.speaking = vec![key(12)];
        state.speaker = Some(key(12));
        let again = shown.choose(&state);
        assert!(again.contains(&key(12)) && again.contains(&key(13)));
        assert_eq!(again.len(), MOST_TILES - 1);
    }

    #[test]
    fn hands_reactions_and_the_chat_show() {
        let mut state = state();
        let bob = ParticipantKey::from_bytes([2; 32]);
        state.participants[0].profile.hand = true;
        state.reactions = vec![mic_core::room::Reacted {
            seq: 4,
            from: bob,
            reaction: mic_core::chat::Reaction::Heart,
        }];
        state.chat = vec![mic_core::room::Message {
            from: state.me.key,
            name: "Alice".to_owned(),
            text: "hi".to_owned(),
        }];
        let shown = view(Some((&state, String::new())));
        let bobs = &shown.tiles[1];
        assert!(bobs.hand);
        assert_eq!(bobs.reaction, Some((mic_core::chat::Reaction::Heart, 4)));
        assert_eq!(shown.chat.len(), 1);
        assert!(shown.chat[0].me);
    }

    #[test]
    fn who_records_is_said() {
        let mut state = state();
        assert_eq!(view(Some((&state, String::new()))).recording_notice, "");
        state.participants[0].profile.recording = true;
        assert_eq!(
            view(Some((&state, String::new()))).recording_notice,
            "Bob is recording this call."
        );
        state.participants[1].profile.recording = true;
        state.me.profile.recording = true;
        let shown = view(Some((&state, String::new())));
        assert_eq!(
            shown.recording_notice,
            "You're recording this call. Bob and Carol are recording this call."
        );
        assert_eq!(shown.recording.len(), 3);
        assert!(shown.recording_self);
    }
}
