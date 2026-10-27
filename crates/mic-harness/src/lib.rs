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

/// What the client shows: the room it's in, if any, and its UI, if it has one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub room: Option<RoomState>,
    /// What the frontend shows; none for a client without one.
    pub ui: Option<Ui>,
}

/// What a frontend shows, as it fills it in: enough for a check to compare with the core's
/// state, whatever the toolkit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ui {
    /// The screen showing: `join`, `room`, …
    pub screen: String,
    /// The tiles on it, by participant key, in the order shown.
    pub tiles: Vec<String>,
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
