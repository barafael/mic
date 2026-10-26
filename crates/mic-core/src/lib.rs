//! mic's core, toolkit-agnostic: rooms and invites, keys, and (from the next commits) the session
//! with the relay, the roster, room state and commands. Frontends only bind to it (PLAN.md, "The
//! core and its frontends").

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

pub mod invite;
pub mod keys;

pub use invite::{Invite, RoomId, RoomSecret};
pub use keys::{DeviceSecret, RoomKey};

/// `N` bytes from the operating system's random generator.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    // The OS's generator failing is a broken system, not something to go on without.
    getrandom::fill(&mut bytes).expect("the OS's random generator works");
    bytes
}
