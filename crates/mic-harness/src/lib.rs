//! The test box's harness: how a check drives a `mic` client, and reads what it shows. One JSON
//! document per line each way over a Unix socket, these types serialized: a client sends
//! [`Request`]s and reads [`Reply`]s, and [`Pushed`] snapshots once subscribed.
//!
//! A frontend embeds the server ([`server`], Unix only) in a few lines with its `mic-core`
//! client, and fills the snapshot's [`Ui`] section, so checks compare what it shows with the
//! core's state without knowing the toolkit. Headless participants (`mic-crowd`) have no UI.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod server;
pub mod synthetic;

use serde::{Deserialize, Serialize};

pub use mic_core::{Command, RoomState};

/// What a harness logs once it listens, which `cargo testbox` waits for.
pub const LISTENING: &str = "the harness is listening";

/// What a frontend prints, on a line of its own, once its window has drawn its first frame,
/// which `cargo testbox` waits for before it looks at the display.
pub const SHOWN: &str = "the window shows";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Echoed in the reply; 0 when a request didn't parse.
    #[serde(default)]
    pub id: u64,
    #[serde(flatten)]
    pub call: Call,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Call {
    /// Answered with [`Pong`]: the client is up.
    Ping,
    /// Answered with [`Snapshot`].
    Snapshot,
    /// Answered with [`Subscribed`], then a [`Pushed`] snapshot on every change.
    Subscribe,
    /// Answered with [`Unsubscribed`].
    Unsubscribe,
    /// Joins the room `invite` names as `name`, leaving any room it's in. Answered with
    /// [`Joined`] once the session is up.
    Join { invite: String, name: String },
    /// Sends `command` to the room it's in, as a click would. Answered with [`Acted`].
    Act { command: Command },
    /// Leaves the room it's in, if any, and forgets it: the client is as it started, for the
    /// next check, without a restart. Answered with [`Done`].
    Reset,
    /// How loud each of `frequencies` (Hz) is in what it played in the last second, for a client
    /// whose speaker is an analyzer (mic-media's `AnalyzingSink`). Answered with [`Heard`].
    Heard { frequencies: Vec<f64> },
    /// What the client decoded of each other participant's camera. Answered with [`Seen`].
    Seen,
}

/// The answer to request `id`: `{"id": …, "result": …}` or `{"id": …, "error": "…"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply<T> {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome<T>,
}

/// A request's result, or why there is none. Tagged by its field: a result that doesn't parse
/// says why, where an untagged reply would only say that no form matched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Outcome<T> {
    #[serde(rename = "result")]
    Ok(T),
    #[serde(rename = "error")]
    Err(String),
}

impl<T> From<Result<T, String>> for Outcome<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(found) => Self::Ok(found),
            Err(error) => Self::Err(error),
        }
    }
}

impl<T> From<Outcome<T>> for Result<T, String> {
    fn from(outcome: Outcome<T>) -> Self {
        match outcome {
            Outcome::Ok(found) => Ok(found),
            Outcome::Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub version: String,
    /// Who the client is in the box (`alice`, `crowd-07`).
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribed {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsubscribed {
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Joined {
    pub state: RoomState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acted {
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub ok: bool,
}

/// What a client heard in the last second: each frequency's amplitude, as asked, and the
/// loudness of it all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heard {
    pub levels: Vec<f64>,
    pub rms: f64,
}

/// What a client decoded of each other participant's camera, in the room's order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub cameras: Vec<Camera>,
    /// Their screens, as decoded, likewise.
    #[serde(default)]
    pub screens: Vec<Camera>,
}

/// One participant's camera, as decoded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Camera {
    /// Whose, by key.
    pub key: String,
    /// How many pictures were decoded.
    pub pictures: u64,
    /// What the last picture's pattern says, if it holds one (mic-media's `read_pattern`): whose
    /// camera it is, and which frame.
    pub sender: Option<u8>,
    pub frame: Option<u16>,
}

/// What the client shows: the room it's in, if any, and its UI, if it has one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub room: Option<RoomState>,
    /// What the frontend shows; none for a client without one.
    pub ui: Option<Ui>,
}

/// What a frontend shows, as it fills it in: enough for a check to compare with the core's
/// state, whatever the toolkit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ui {
    /// The screen showing: `join`, `room`, …
    pub screen: String,
    /// The tiles on it, by participant key, in the order shown.
    pub tiles: Vec<String>,
    /// The tiles that show their participant muted, by key.
    #[serde(default)]
    pub muted: Vec<String>,
    /// The tiles that say they can't show their participant's video, by key.
    #[serde(default)]
    pub cant_decode: Vec<String>,
    /// What drawing the tiles' video costs, over the last second, if the frontend says.
    #[serde(default)]
    pub render: Option<Render>,
    /// The tiles showing their participant's video, by key, as of the last second: the others',
    /// not its own camera's preview.
    #[serde(default)]
    pub video: Vec<String>,
    /// What the window says of the version, if anything: a newer one is out.
    #[serde(default)]
    pub notice: Option<String>,
    /// The link the window asks whether to leave the call for, while it asks.
    #[serde(default)]
    pub asking: Option<String>,
    /// The participants not given a tile, beyond those shown: the "+N" tile.
    #[serde(default)]
    pub overflow: usize,
    /// The tiles ringed as speaking, by key.
    #[serde(default)]
    pub speaking: Vec<String>,
    /// The tiles showing a raised hand, by key.
    #[serde(default)]
    pub hands: Vec<String>,
    /// Each tile's latest reaction, as `<key> <reaction>`, the reaction as the core names it.
    #[serde(default)]
    pub reactions: Vec<String>,
    /// The chat, as `<name>: <text>`, oldest first.
    #[serde(default)]
    pub chat: Vec<String>,
    /// The others' shared screens it shows, by key, the one on the stage first.
    #[serde(default)]
    pub screens: Vec<String>,
    /// Whether it shares its own screen.
    #[serde(default)]
    pub sharing: bool,
    /// What it says of who records the call, if anyone does: the indicator (D7).
    #[serde(default)]
    pub recording: Option<String>,
    /// Who it shows as recording, by key, itself among them if it does.
    #[serde(default)]
    pub recorders: Vec<String>,
    /// Whether it shows itself alone in the room: the invite offered, its own tile small.
    #[serde(default)]
    pub alone: bool,
    /// Everyone its participants panel lists, by key, itself first.
    #[serde(default)]
    pub people: Vec<String>,
    /// Those the panel shows with a hand raised, by key, whether they have a tile or not.
    #[serde(default)]
    pub people_hands: Vec<String>,
    /// The tiles shown greyed, away while the room redials, by key.
    #[serde(default)]
    pub away: Vec<String>,
    /// Whether it says the call goes over the WebSocket fallback.
    #[serde(default)]
    pub fallback: bool,
    /// The room's name it shows, if the room has one.
    #[serde(default)]
    pub room_name: Option<String>,
    /// The name of the room it asks about leaving for, while it asks, if the link names one.
    #[serde(default)]
    pub asking_name: Option<String>,
    /// What the join screen's name field holds: the name it joins with next.
    #[serde(default)]
    pub name: String,
}

/// What drawing a frontend's video tiles costs, over the last second: the others' cameras, not
/// its own camera's preview.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Render {
    /// Tiles showing video.
    pub video_tiles: u32,
    /// Pictures drawn a second, all tiles together.
    pub pictures: f64,
    /// Drawing one into its tile, on average, in ms.
    pub draw_ms: f64,
    /// The window's frame, on average, in ms, if the renderer says.
    pub frame_ms: Option<f64>,
}

/// Pushed to a subscriber on every change of what it shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pushed {
    pub seq: u64,
    pub snapshot: Snapshot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_read_as_documented() {
        let request: Request = serde_json::from_str(
            r#"{"id": 3, "method": "act", "command": {"command": "set_name", "name": "Bob"}}"#,
        )
        .unwrap();
        assert_eq!(
            request,
            Request {
                id: 3,
                call: Call::Act {
                    command: Command::SetName {
                        name: "Bob".to_owned()
                    }
                },
            }
        );
        let reply = Reply {
            id: 3,
            outcome: Outcome::<Acted>::Err("no room".to_owned()),
        };
        assert_eq!(
            serde_json::to_string(&reply).unwrap(),
            r#"{"id":3,"error":"no room"}"#
        );
    }
}
