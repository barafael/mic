//! mic's core, toolkit-agnostic: rooms and invites, keys, the device secret in the keyring, the
//! session with the relay, the roster, room state and commands, and the room a frontend is in.
//! Frontends only bind to it (PLAN.md, "The core and its frontends").

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

pub mod app;
pub mod catalog;
pub mod invite;
pub mod keys;
pub mod room;
pub mod roster;
pub mod secrets;
pub mod session;
pub mod sig;
pub mod tracks;

pub use app::App;
pub use invite::{Invite, RoomId, RoomSecret};
pub use keys::{DeviceSecret, RoomKey};
pub use room::{Command, Connection, Event, Room, RoomState};
pub use roster::{Participant, ParticipantKey, Profile, Roster};
pub use secrets::{SecretStore, device_secret};
pub use session::{Client, Config, Trust};

/// `N` bytes from the operating system's random generator.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    // The OS's generator failing is a broken system, not something to go on without.
    getrandom::fill(&mut bytes).expect("the OS's random generator works");
    bytes
}
